//! VMP runtime 的 LLVM IR emitter。
//!
//! # 生成内容
//! - `read_varint/read_operand`：按 `decoder.vm` 描述的 pipeline 解码 code segment。
//! - `read_const/read_const_varint`：按 `bytecode.vm` 的 const_pool 契约读取常量池。
//! - `dispatch`：固定寄存器 VM 的主循环，维护 `x0..x31`、`q0..q64` 和 `pc`。
//!
//! # profile 驱动边界
//! handler 的 opcode、operand 顺序和 semantic program 来自 profile。此文件只把已校验的
//! semantic AST 匹配到当前支持的有限 handler template，并据此生成 LLVM IR；不会执行 profile 文本。

use amice_llvm::inkwell2::BuilderExt;
use amice_llvm::ptr_type;
use amice_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use amice_plugin::inkwell::basic_block::BasicBlock;
use amice_plugin::inkwell::llvm_sys::core::{LLVMBuildFence, LLVMSetAlignment};
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::types::{ArrayType, FunctionType, IntType, PointerType};
use amice_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValue, FloatValue, FunctionValue, IntValue, PointerValue, UnnamedAddress,
};
use amice_plugin::inkwell::{
    AddressSpace, AtomicOrdering, AtomicRMWBinOp, FloatPredicate as LlvmFloatPredicate, IntPredicate,
};
use amice_vm::isa::{
    AtomicRmwOp, BinOp, CastOp, FloatBinOp, FloatCastOp, FloatPredicate as VmFloatPredicate, FloatUnaryOp,
    InstructionDesc, IntTernaryOp, IntUnaryOp, MemoryOrdering, Opcode, PcExpr, SemanticAtomicRmwOp, SemanticBinOp,
    SemanticExpr, SemanticFloatBinOp, SemanticFloatCastOp, SemanticFloatUnaryOp, SemanticIntTernaryOp,
    SemanticIntUnaryOp, SemanticProgram, SemanticStmt,
};
use amice_vm::profile::DecoderStep;
use amice_vm::{NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, ProfilePackage, RuntimeScope};
use anyhow::Context;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub struct RuntimeFunctions<'ctx> {
    // wrapper 调用的 VM dispatcher。其它 helper 通过 internal symbol 被 dispatcher 引用。
    pub dispatch: FunctionValue<'ctx>,
}

pub fn emit_runtime<'ctx>(
    module: &mut Module<'ctx>,
    profile: &ProfilePackage,
    scope: RuntimeScope,
    symbol_suffix: &str,
) -> anyhow::Result<RuntimeFunctions<'ctx>> {
    // func scope 或 per-function clone 会给 runtime 符号加函数后缀；module scope 则复用同一组 helper。
    // 这样 runtime scope 与 handler clone 策略只影响符号复用，不改变 dispatcher ABI。
    let suffix = match (scope, profile.runtime.enhancements.handler_clone) {
        (_, amice_vm::runtime::HandlerClonePolicy::PerFunction) | (RuntimeScope::Func, _) => {
            format!(".{symbol_suffix}")
        },
        (RuntimeScope::Module, amice_vm::runtime::HandlerClonePolicy::Disabled) => String::new(),
    };
    let read_name = format!(".amice.vm.read_varint{suffix}");
    let read_operand_name = format!(".amice.vm.read_operand{suffix}");
    let read_const_varint_name = format!(".amice.vm.read_const_varint{suffix}");
    let read_const_name = format!(".amice.vm.read_const{suffix}");
    let dispatch_name = format!(".amice.vm.dispatch{suffix}");

    let read_varint = match module.get_function(&read_name) {
        Some(function) => function,
        None => emit_read_varint(module, profile, &read_name)?,
    };
    let read_operand = match module.get_function(&read_operand_name) {
        Some(function) => function,
        None => emit_read_operand(module, profile, read_varint, &read_operand_name)?,
    };
    let read_const_varint = match module.get_function(&read_const_varint_name) {
        Some(function) => function,
        None => emit_read_const_varint(module, &read_const_varint_name)?,
    };
    let read_const = match module.get_function(&read_const_name) {
        Some(function) => function,
        None => emit_read_const_pool(module, read_const_varint, &read_const_name)?,
    };

    let dispatch = match module.get_function(&dispatch_name) {
        Some(function) => function,
        None => emit_dispatch(module, profile, read_varint, read_operand, read_const, &dispatch_name)?,
    };

    Ok(RuntimeFunctions { dispatch })
}

#[derive(Clone, Copy)]
enum RuntimeInlinePolicy {
    Normal,
    Always,
}

fn add_private_runtime_function<'ctx>(
    module: &mut Module<'ctx>,
    name: &str,
    fn_type: FunctionType<'ctx>,
    inline_policy: RuntimeInlinePolicy,
) -> FunctionValue<'ctx> {
    let function = module.add_function(name, fn_type, Some(Linkage::Private));
    function.as_global_value().set_unnamed_address(UnnamedAddress::Global);

    if matches!(inline_policy, RuntimeInlinePolicy::Always) {
        // reader helper 是 runtime 的机械解码片段，暴露成独立符号只会让 IDA 里多出
        // `amice_vm_read_*` 这类可读入口；交给后续 always-inline pass 尽量折进 dispatcher。
        let ctx = module.get_context();
        let always_inline = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        function.add_attribute(AttributeLoc::Function, always_inline);
        function.remove_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id("noinline"));
        function.remove_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id("optnone"));
    }

    function
}

fn emit_read_varint<'ctx>(
    module: &mut Module<'ctx>,
    profile: &ProfilePackage,
    name: &str,
) -> anyhow::Result<FunctionValue<'ctx>> {
    // code stream 的最小 token 是 varint。每读取一个 raw byte，都先按 profile decoder pipeline
    // 逆向还原，再参与 varint 拼接。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let fn_type = i64_type.fn_type(
        &[ptr_type.into(), i64_type.into(), i64_type.into(), ptr_type.into()],
        false,
    );
    let function = add_private_runtime_function(module, name, fn_type, RuntimeInlinePolicy::Always);
    let builder = ctx.create_builder();

    let entry = ctx.append_basic_block(function, "entry");
    let loop_block = ctx.append_basic_block(function, "loop");
    let read_block = ctx.append_basic_block(function, "read");
    let done_block = ctx.append_basic_block(function, "done");

    builder.position_at_end(entry);
    let result_ptr = builder.build_alloca(i64_type, "result")?;
    let shift_ptr = builder.build_alloca(i64_type, "shift")?;
    builder.build_store(result_ptr, i64_type.const_zero())?;
    builder.build_store(shift_ptr, i64_type.const_zero())?;
    builder.build_unconditional_branch(loop_block)?;

    let code = function.get_nth_param(0).unwrap().into_pointer_value();
    let len = function.get_nth_param(1).unwrap().into_int_value();
    let key = function.get_nth_param(2).unwrap().into_int_value();
    let offset_ptr = function.get_nth_param(3).unwrap().into_pointer_value();

    builder.position_at_end(loop_block);
    let offset = load_i64(&builder, i64_type, offset_ptr, "offset")?;
    let in_bounds = builder.build_int_compare(IntPredicate::ULT, offset, len, "in_bounds")?;
    builder.build_conditional_branch(in_bounds, read_block, done_block)?;

    builder.position_at_end(read_block);
    let byte_ptr = builder.build_gep2(i8_type, code, &[offset], "byte.ptr")?;
    let raw = builder.build_load2(i8_type, byte_ptr, "raw.byte")?.into_int_value();
    let next_offset = builder.build_int_add(offset, i64_type.const_int(1, false), "next.offset")?;
    builder.build_store(offset_ptr, next_offset)?;

    let decoded = decode_byte_from_profile(&builder, profile, i8_type, i64_type, raw, key, offset)?;
    let decoded64 = builder.build_int_z_extend(decoded, i64_type, "decoded64")?;
    let payload = builder.build_and(decoded64, i64_type.const_int(0x7f, false), "payload")?;
    let shift = load_i64(&builder, i64_type, shift_ptr, "shift")?;
    let shifted = builder.build_left_shift(payload, shift, "payload.shifted")?;
    let old_result = load_i64(&builder, i64_type, result_ptr, "result.old")?;
    let new_result = builder.build_or(old_result, shifted, "result.new")?;
    builder.build_store(result_ptr, new_result)?;
    let next_shift = builder.build_int_add(shift, i64_type.const_int(7, false), "shift.next")?;
    builder.build_store(shift_ptr, next_shift)?;
    let cont = builder.build_and(decoded64, i64_type.const_int(0x80, false), "cont.bit")?;
    let has_more = builder.build_int_compare(IntPredicate::NE, cont, i64_type.const_zero(), "has.more")?;
    builder.build_conditional_branch(has_more, loop_block, done_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(&builder, i64_type, result_ptr, "result")?;
    builder.build_return(Some(&result))?;

    Ok(function)
}

fn emit_read_operand<'ctx>(
    module: &mut Module<'ctx>,
    profile: &ProfilePackage,
    read_varint: FunctionValue<'ctx>,
    name: &str,
) -> anyhow::Result<FunctionValue<'ctx>> {
    if !profile.decoder.steps.contains(&DecoderStep::BitUnpack) {
        return Ok(read_varint);
    }

    // BitUnpack 的 encoder 侧会把 operand 写成“bit_width + 若干 7-bit chunk”。
    // runtime 读取 operand 时必须先拿 bit_width，再连续消费 chunk 还原完整 u64。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let fn_type = i64_type.fn_type(
        &[ptr_type.into(), i64_type.into(), i64_type.into(), ptr_type.into()],
        false,
    );
    let function = add_private_runtime_function(module, name, fn_type, RuntimeInlinePolicy::Always);
    let builder = ctx.create_builder();

    let entry = ctx.append_basic_block(function, "entry");
    let loop_block = ctx.append_basic_block(function, "loop");
    let read_block = ctx.append_basic_block(function, "read");
    let done_block = ctx.append_basic_block(function, "done");

    builder.position_at_end(entry);
    let result_ptr = builder.build_alloca(i64_type, "operand.result")?;
    let shift_ptr = builder.build_alloca(i64_type, "operand.shift")?;
    builder.build_store(result_ptr, i64_type.const_zero())?;
    builder.build_store(shift_ptr, i64_type.const_zero())?;

    let code = function.get_nth_param(0).unwrap().into_pointer_value();
    let len = function.get_nth_param(1).unwrap().into_int_value();
    let key = function.get_nth_param(2).unwrap().into_int_value();
    let offset_ptr = function.get_nth_param(3).unwrap().into_pointer_value();
    let bit_width = call_reader(&builder, read_varint, code, len, key, offset_ptr, "operand.bit_width")?;
    let has_bits = builder.build_int_compare(IntPredicate::NE, bit_width, i64_type.const_zero(), "operand.has_bits")?;
    builder.build_conditional_branch(has_bits, loop_block, done_block)?;

    builder.position_at_end(loop_block);
    let shift = load_i64(&builder, i64_type, shift_ptr, "operand.shift.cur")?;
    let within_width = builder.build_int_compare(IntPredicate::ULT, shift, bit_width, "operand.within_width")?;
    let within_u64 = builder.build_int_compare(
        IntPredicate::ULT,
        shift,
        i64_type.const_int(64, false),
        "operand.within_u64",
    )?;
    let keep_reading = builder.build_and(within_width, within_u64, "operand.keep_reading")?;
    builder.build_conditional_branch(keep_reading, read_block, done_block)?;

    builder.position_at_end(read_block);
    let chunk = call_reader(&builder, read_varint, code, len, key, offset_ptr, "operand.chunk")?;
    let masked = builder.build_and(chunk, i64_type.const_int(0x7f, false), "operand.chunk.masked")?;
    let shifted = builder.build_left_shift(masked, shift, "operand.chunk.shifted")?;
    let old_result = load_i64(&builder, i64_type, result_ptr, "operand.result.old")?;
    let new_result = builder.build_or(old_result, shifted, "operand.result.new")?;
    builder.build_store(result_ptr, new_result)?;
    let next_shift = builder.build_int_add(shift, i64_type.const_int(7, false), "operand.shift.next")?;
    builder.build_store(shift_ptr, next_shift)?;
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(&builder, i64_type, result_ptr, "operand.result.final")?;
    builder.build_return(Some(&result))?;

    Ok(function)
}

fn emit_read_const_varint<'ctx>(module: &mut Module<'ctx>, name: &str) -> anyhow::Result<FunctionValue<'ctx>> {
    // const_pool 不走 code decoder pipeline，只使用 const_pool 自己的 XOR key stream。
    // 这里仍采用 varint，是因为常量池格式本身由 bytecode.vm 约束为变长整数序列。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let fn_type = i64_type.fn_type(
        &[ptr_type.into(), i64_type.into(), i64_type.into(), ptr_type.into()],
        false,
    );
    let function = add_private_runtime_function(module, name, fn_type, RuntimeInlinePolicy::Always);
    let builder = ctx.create_builder();

    let entry = ctx.append_basic_block(function, "entry");
    let loop_block = ctx.append_basic_block(function, "loop");
    let read_block = ctx.append_basic_block(function, "read");
    let done_block = ctx.append_basic_block(function, "done");

    builder.position_at_end(entry);
    let result_ptr = builder.build_alloca(i64_type, "const.result")?;
    let shift_ptr = builder.build_alloca(i64_type, "const.shift")?;
    builder.build_store(result_ptr, i64_type.const_zero())?;
    builder.build_store(shift_ptr, i64_type.const_zero())?;
    builder.build_unconditional_branch(loop_block)?;

    let bytes = function.get_nth_param(0).unwrap().into_pointer_value();
    let len = function.get_nth_param(1).unwrap().into_int_value();
    let key = function.get_nth_param(2).unwrap().into_int_value();
    let offset_ptr = function.get_nth_param(3).unwrap().into_pointer_value();

    builder.position_at_end(loop_block);
    let offset = load_i64(&builder, i64_type, offset_ptr, "const.offset")?;
    let in_bounds = builder.build_int_compare(IntPredicate::ULT, offset, len, "const.in_bounds")?;
    builder.build_conditional_branch(in_bounds, read_block, done_block)?;

    builder.position_at_end(read_block);
    let byte_ptr = builder.build_gep2(i8_type, bytes, &[offset], "const.byte.ptr")?;
    let raw = builder
        .build_load2(i8_type, byte_ptr, "const.raw.byte")?
        .into_int_value();
    let next_offset = builder.build_int_add(offset, i64_type.const_int(1, false), "const.next.offset")?;
    builder.build_store(offset_ptr, next_offset)?;

    // const_pool segment 有自己的 bytecode.vm 加密契约，不能用 decoder.vm 的 code pipeline
    // 解码；那些 step 描述的是指令 record，而不是任意常量数据。
    let key_byte = key_stream_byte(&builder, i8_type, i64_type, key, offset)?;
    let decoded = builder.build_xor(raw, key_byte, "const.decoded.xor")?;
    let decoded64 = builder.build_int_z_extend(decoded, i64_type, "const.decoded64")?;
    let payload = builder.build_and(decoded64, i64_type.const_int(0x7f, false), "const.payload")?;
    let shift = load_i64(&builder, i64_type, shift_ptr, "const.shift")?;
    let shifted = builder.build_left_shift(payload, shift, "const.payload.shifted")?;
    let old_result = load_i64(&builder, i64_type, result_ptr, "const.result.old")?;
    let new_result = builder.build_or(old_result, shifted, "const.result.new")?;
    builder.build_store(result_ptr, new_result)?;
    let next_shift = builder.build_int_add(shift, i64_type.const_int(7, false), "const.shift.next")?;
    builder.build_store(shift_ptr, next_shift)?;
    let cont = builder.build_and(decoded64, i64_type.const_int(0x80, false), "const.cont.bit")?;
    let has_more = builder.build_int_compare(IntPredicate::NE, cont, i64_type.const_zero(), "const.has.more")?;
    builder.build_conditional_branch(has_more, loop_block, done_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(&builder, i64_type, result_ptr, "const.result")?;
    builder.build_return(Some(&result))?;

    Ok(function)
}

fn emit_read_const_pool<'ctx>(
    module: &mut Module<'ctx>,
    read_const_varint: FunctionValue<'ctx>,
    name: &str,
) -> anyhow::Result<FunctionValue<'ctx>> {
    // const_pool layout 是：count 后跟 count 个 varint。runtime 按 index 线性扫描，
    // 这样 const_pool index 可以保持小整数 operand，不需要把大常量塞进 code stream。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let fn_type = i64_type.fn_type(
        &[ptr_type.into(), i64_type.into(), i64_type.into(), i64_type.into()],
        false,
    );
    let function = add_private_runtime_function(module, name, fn_type, RuntimeInlinePolicy::Always);
    let builder = ctx.create_builder();

    let entry = ctx.append_basic_block(function, "entry");
    let loop_block = ctx.append_basic_block(function, "loop");
    let found_block = ctx.append_basic_block(function, "found");
    let advance_block = ctx.append_basic_block(function, "advance");
    let done_block = ctx.append_basic_block(function, "done");

    builder.position_at_end(entry);
    let offset_ptr = builder.build_alloca(i64_type, "const.pool.offset")?;
    let current_ptr = builder.build_alloca(i64_type, "const.pool.current")?;
    let result_ptr = builder.build_alloca(i64_type, "const.pool.result")?;
    builder.build_store(offset_ptr, i64_type.const_zero())?;
    builder.build_store(current_ptr, i64_type.const_zero())?;
    builder.build_store(result_ptr, i64_type.const_zero())?;

    let bytes = function.get_nth_param(0).unwrap().into_pointer_value();
    let len = function.get_nth_param(1).unwrap().into_int_value();
    let key = function.get_nth_param(2).unwrap().into_int_value();
    let target = function.get_nth_param(3).unwrap().into_int_value();
    let count = call_reader(
        &builder,
        read_const_varint,
        bytes,
        len,
        key,
        offset_ptr,
        "const.pool.count",
    )?;
    let in_bounds = builder.build_int_compare(IntPredicate::ULT, target, count, "const.pool.index.ok")?;
    builder.build_conditional_branch(in_bounds, loop_block, done_block)?;

    builder.position_at_end(loop_block);
    let value = call_reader(
        &builder,
        read_const_varint,
        bytes,
        len,
        key,
        offset_ptr,
        "const.pool.value",
    )?;
    let current = load_i64(&builder, i64_type, current_ptr, "const.pool.current.value")?;
    let is_target = builder.build_int_compare(IntPredicate::EQ, current, target, "const.pool.is.target")?;
    builder.build_conditional_branch(is_target, found_block, advance_block)?;

    builder.position_at_end(found_block);
    builder.build_store(result_ptr, value)?;
    builder.build_unconditional_branch(done_block)?;

    builder.position_at_end(advance_block);
    let next = builder.build_int_add(current, i64_type.const_int(1, false), "const.pool.current.next")?;
    builder.build_store(current_ptr, next)?;
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(&builder, i64_type, result_ptr, "const.pool.result")?;
    builder.build_return(Some(&result))?;

    Ok(function)
}

fn emit_dispatch<'ctx>(
    module: &mut Module<'ctx>,
    profile: &ProfilePackage,
    read_varint: FunctionValue<'ctx>,
    read_operand: FunctionValue<'ctx>,
    read_const: FunctionValue<'ctx>,
    name: &str,
) -> anyhow::Result<FunctionValue<'ctx>> {
    // dispatcher ABI 固定为一组 i64 和指针，避免每个被保护函数都生成不同签名。
    // wrapper 负责把原函数签名适配到这个 ABI，dispatcher 只执行 VM bytecode。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let mut params = Vec::with_capacity(17);
    params.push(ptr_type.into());
    params.push(i64_type.into());
    params.push(ptr_type.into());
    params.push(i64_type.into());
    params.push(i64_type.into());
    params.push(i64_type.into());
    params.push(ptr_type.into());
    params.push(i64_type.into());
    params.push(ptr_type.into());
    for _ in 0..8 {
        params.push(i64_type.into());
    }
    let fn_type = i64_type.fn_type(&params, false);
    let function = add_private_runtime_function(module, name, fn_type, RuntimeInlinePolicy::Normal);
    let builder = ctx.create_builder();
    let x_type = i64_type.array_type(32);
    let q_lane_type = i8_type.vec_type(16);
    let q_type = q_lane_type.array_type(65);

    let entry = ctx.append_basic_block(function, "entry");
    let loop_check = ctx.append_basic_block(function, "loop.check");
    let execute_decode = ctx.append_basic_block(function, "execute.decode");
    let default_return = ctx.append_basic_block(function, "default.return");

    builder.position_at_end(entry);
    let regs = builder.build_alloca(x_type, "x")?;
    // 即使内置 profile 声明 q.lowering = disabled，runtime state 仍保留固定 q0..q64 组。
    // 契约是 verifier 拒绝不支持的宽值 lowering，而不是让 VM 悄悄改变形状并丢掉 v128 寄存器组。
    let _q_regs = builder.build_alloca(q_type, "q")?;
    let pc_ptr = builder.build_alloca(i64_type, "pc")?;
    let offset_ptr = builder.build_alloca(i64_type, "offset")?;
    builder.build_store(pc_ptr, i64_type.const_zero())?;

    // wrapper 总是传 8 个 i64 参数槽；profile ABI 决定这些槽落到哪些 x 寄存器。
    for index in 0..8 {
        let target_reg = profile.abi.integer_args.get(index).copied().unwrap_or(index as u8);
        let value = function
            .get_nth_param((9 + index) as u32)
            .expect("dispatch arg should exist")
            .into_int_value();
        store_reg(
            &builder,
            i64_type,
            x_type,
            regs,
            i64_type.const_int(target_reg as u64, false),
            value,
        )?;
    }
    builder.build_unconditional_branch(loop_check)?;

    let code = function.get_nth_param(0).unwrap().into_pointer_value();
    let len = function.get_nth_param(1).unwrap().into_int_value();
    let const_pool = function.get_nth_param(2).unwrap().into_pointer_value();
    let const_pool_len = function.get_nth_param(3).unwrap().into_int_value();
    let key = function.get_nth_param(4).unwrap().into_int_value();
    let pc_limit = function.get_nth_param(5).unwrap().into_int_value();
    let native_table = function.get_nth_param(6).unwrap().into_pointer_value();
    let native_count = function.get_nth_param(7).unwrap().into_int_value();
    let ret_slots = function.get_nth_param(8).unwrap().into_pointer_value();
    let return_reg = profile
        .abi
        .integer_returns
        .first()
        .copied()
        .context("verified ABI must declare ret0 mapping")?;
    let return_regs = profile.abi.integer_returns.as_slice();
    let lr_register = alias_x_register(profile, &profile.abi.lr_alias)?;

    builder.position_at_end(loop_check);
    let pc = load_i64(&builder, i64_type, pc_ptr, "pc")?;
    let pc_in_range = builder.build_int_compare(IntPredicate::ULT, pc, pc_limit, "pc.in.range")?;
    builder.build_store(offset_ptr, pc)?;
    builder.build_conditional_branch(pc_in_range, execute_decode, default_return)?;

    let handler_alias_order = handler_alias_order(profile, name);

    builder.position_at_end(execute_decode);
    let opcode = read_token(
        &builder,
        read_varint,
        RuntimeArgs {
            code,
            len,
            key,
            offset_ptr,
        },
        "opcode",
    )?;
    let execute_case_count = profile.isa.instructions.iter().map(|desc| desc.opcodes().len()).sum();
    let mut execute_cases = Vec::with_capacity(execute_case_count);
    // 执行路径按具体 opcode alias switch 到 handler clone。同一语义的不同 alias 也拥有
    // 独立 block，避免跳转表一眼聚类成“28 个真实 handler + 其余 default”。
    for (instruction_index, opcode) in &handler_alias_order {
        let desc = &profile.isa.instructions[*instruction_index];
        let (case_block, body_block) = if profile.runtime.enhancements.handler_splitting {
            let entry = ctx.append_basic_block(function, &format!("handler.{}.op{opcode:02x}.split.entry", desc.name));
            let body = ctx.append_basic_block(function, &format!("handler.{}.op{opcode:02x}.split.body", desc.name));
            (entry, body)
        } else {
            let block = ctx.append_basic_block(function, &format!("handler.{}.op{opcode:02x}", desc.name));
            (block, block)
        };
        execute_cases.push((i64_type.const_int(*opcode as u64, false), case_block));
        if profile.runtime.enhancements.handler_splitting {
            builder.position_at_end(case_block);
            builder.build_unconditional_branch(body_block)?;
        }
        builder.position_at_end(body_block);
        let operands = read_handler_operands(
            &builder,
            read_operand,
            RuntimeArgs {
                code,
                len,
                key,
                offset_ptr,
            },
            desc,
        )?;
        emit_handler(
            &builder,
            operands,
            HandlerContext {
                function,
                i8_type,
                i64_type,
                ptr_type,
                x_type,
                regs,
                pc_ptr,
                loop_check,
                native_table,
                native_count,
                read_const,
                const_pool,
                const_pool_len,
                key,
                return_reg,
                return_regs,
                lr_register,
                ret_slots,
                decoded_width: desc.decoded_width,
            },
            &desc.semantic_program,
        )?;
    }
    builder.position_at_end(execute_decode);
    builder.build_switch(opcode, default_return, &execute_cases)?;

    builder.position_at_end(default_return);
    let ret = load_reg(
        &builder,
        i64_type,
        x_type,
        regs,
        i64_type.const_int(return_reg as u64, false),
        "ret",
    )?;
    store_return_slots(
        &builder,
        HandlerContext {
            function,
            i8_type,
            i64_type,
            ptr_type,
            x_type,
            regs,
            pc_ptr,
            loop_check,
            native_table,
            native_count,
            read_const,
            const_pool,
            const_pool_len,
            key,
            return_reg,
            return_regs,
            lr_register,
            ret_slots,
            decoded_width: 0,
        },
    )?;
    builder.build_return(Some(&ret))?;

    Ok(function)
}

#[derive(Clone, Copy)]
struct RuntimeArgs<'ctx> {
    // code segment 起点。
    code: PointerValue<'ctx>,
    // code segment 长度，用于 reader 边界检查。
    len: IntValue<'ctx>,
    // per-function 或 module key，供 decoder stream transform 使用。
    key: IntValue<'ctx>,
    // 当前 code offset 指针；reader 会原地推进它。
    offset_ptr: PointerValue<'ctx>,
}

#[derive(Clone, Copy)]
struct HandlerContext<'ctx, 'profile> {
    // 当前 dispatcher 函数，native-call handler 需要它来创建 call 指令。
    function: FunctionValue<'ctx>,
    i8_type: IntType<'ctx>,
    i64_type: IntType<'ctx>,
    ptr_type: PointerType<'ctx>,
    x_type: ArrayType<'ctx>,
    regs: PointerValue<'ctx>,
    // VM pc 状态。branch/ret handler 都通过写它控制下一轮 dispatch。
    pc_ptr: PointerValue<'ctx>,
    // handler 执行完后回到 loop_check，重新验证 pc 是否仍在 bytecode 范围内。
    loop_check: BasicBlock<'ctx>,
    native_table: PointerValue<'ctx>,
    native_count: IntValue<'ctx>,
    read_const: FunctionValue<'ctx>,
    const_pool: PointerValue<'ctx>,
    const_pool_len: IntValue<'ctx>,
    key: IntValue<'ctx>,
    return_reg: u8,
    // 多返回值/aggregate return 使用的 ABI 返回寄存器列表。
    return_regs: &'profile [u8],
    // profile alias 解析后的 lr 寄存器，用于 VM 内部 call/ret。
    lr_register: u8,
    // wrapper 分配的返回槽数组，aggregate/sret 路径通过它带出多个值。
    ret_slots: PointerValue<'ctx>,
    // 当前 handler 对应的 decoded record 字节宽度，来自 profile `decoded_width`。
    decoded_width: u8,
}

#[derive(Debug)]
struct HandlerOperands<'ctx> {
    values: HashMap<String, IntValue<'ctx>>,
}

impl<'ctx> HandlerOperands<'ctx> {
    fn get(&self, name: &str) -> anyhow::Result<IntValue<'ctx>> {
        self.values
            .get(name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("handler operand {name} was not decoded"))
    }
}

fn read_handler_operands<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    read_varint: FunctionValue<'ctx>,
    args: RuntimeArgs<'ctx>,
    desc: &InstructionDesc,
) -> anyhow::Result<HandlerOperands<'ctx>> {
    let mut values = HashMap::with_capacity(desc.operand_descs.len());
    for operand in &desc.operand_descs {
        values.insert(
            operand.name.clone(),
            read_token(builder, read_varint, args, &format!("{}.{}", desc.name, operand.name))?,
        );
    }
    Ok(HandlerOperands { values })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeHandlerTemplate {
    MovImm,
    ConstLoad,
    SuperAddXor,
    SuperIcmpBrIf,
    SuperGepLoad,
    SuperLoadAdd,
    Mov,
    Bin(BinOp),
    IntUnary(IntUnaryOp),
    IntTernary(IntTernaryOp),
    FloatBin(FloatBinOp),
    FloatUnary(FloatUnaryOp),
    FloatCast(FloatCastOp),
    Icmp,
    Fcmp,
    Cast(CastOp),
    Alloca,
    Load,
    Store,
    AtomicLoad,
    AtomicStore,
    AtomicRmw(AtomicRmwOp),
    CmpXchg,
    Fence,
    Gep,
    CallNative,
    Nop,
    Br,
    BrCond,
    VmCall,
    VmRet,
    Ret,
}

impl RuntimeHandlerTemplate {
    fn from_program(program: &SemanticProgram) -> anyhow::Result<Self> {
        // profile 的 semantic block 已经被解析成 AST。这里做结构匹配而不是看指令名，
        // 所以用户可以改名或换 opcode，只要语义仍落在当前支持的 handler template 内。
        let statements = &program.statements;
        let template = if has_assign_reg(statements, "dst", &trunc_width(operand("imm"), operand("width"))) {
            Self::MovImm
        } else if has_assign_reg(statements, "dst", &SemanticExpr::ConstPool("index".to_owned())) {
            Self::ConstLoad
        } else if has_assign_reg(statements, "dst", &trunc_width(reg("src"), operand("width"))) {
            Self::Mov
        } else if add_xor_template(statements) {
            Self::SuperAddXor
        } else if icmp_br_if_template(statements) {
            Self::SuperIcmpBrIf
        } else if gep_load_template(statements) {
            Self::SuperGepLoad
        } else if load_add_template(statements) {
            Self::SuperLoadAdd
        } else if let Some(op) = bin_template(statements) {
            Self::Bin(op)
        } else if ashr_template(statements) {
            Self::Bin(BinOp::AShr)
        } else if let Some(op) = int_unary_template(statements) {
            Self::IntUnary(op)
        } else if let Some(op) = int_ternary_template(statements) {
            Self::IntTernary(op)
        } else if let Some(op) = float_bin_template(statements) {
            Self::FloatBin(op)
        } else if let Some(op) = float_unary_template(statements) {
            Self::FloatUnary(op)
        } else if let Some(op) = float_cast_template(statements) {
            Self::FloatCast(op)
        } else if has_assign_reg(statements, "dst", &compare_expr()) {
            Self::Icmp
        } else if has_assign_reg(statements, "dst", &float_compare_expr()) {
            Self::Fcmp
        } else if has_assign_reg(statements, "dst", &zero_extend_expr()) {
            Self::Cast(CastOp::ZExt)
        } else if has_assign_reg(statements, "dst", &sign_extend_expr()) {
            Self::Cast(CastOp::SExt)
        } else if has_assign_reg(statements, "dst", &trunc_width(reg("src"), operand("to_width"))) {
            Self::Cast(CastOp::Trunc)
        } else if has_assign_reg(statements, "dst", &bitcast_expr()) {
            Self::Cast(CastOp::Bitcast)
        } else if has_assign_reg(statements, "dst", &stack_alloc_expr()) {
            Self::Alloca
        } else if has_assign_reg(statements, "dst", &load_width_expr()) {
            Self::Load
        } else if has_assign_reg(statements, "dst", &atomic_load_width_expr()) {
            Self::AtomicLoad
        } else if store_template(statements) {
            Self::Store
        } else if atomic_store_template(statements) {
            Self::AtomicStore
        } else if let Some(op) = atomic_rmw_template(statements) {
            Self::AtomicRmw(op)
        } else if cmpxchg_template(statements) {
            Self::CmpXchg
        } else if fence_template(statements) {
            Self::Fence
        } else if has_assign_reg(statements, "dst", &gep_expr()) {
            Self::Gep
        } else if call_native_template(statements) {
            Self::CallNative
        } else if statements
            .iter()
            .any(|stmt| matches!(stmt, SemanticStmt::StateUnchanged))
        {
            Self::Nop
        } else if has_assign_reg(statements, "lr", &SemanticExpr::NextPc) && pc_label(statements, "target") {
            Self::VmCall
        } else if pc_register(statements, "lr") {
            Self::VmRet
        } else if pc_label(statements, "target") {
            Self::Br
        } else if pc_select_template(statements) {
            Self::BrCond
        } else if has_assign_reg(statements, "ret0", &reg("src")) && pc_return(statements) {
            Self::Ret
        } else {
            anyhow::bail!("semantic AST does not match a supported runtime handler template");
        };

        Ok(template)
    }
}

fn emit_handler<'ctx, 'profile>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, 'profile>,
    semantic: &SemanticProgram,
) -> anyhow::Result<()> {
    let template = RuntimeHandlerTemplate::from_program(semantic)?;
    // 所有 handler 都遵守同一条规则：只读已解码 operand 和 VM state，写回 x/pc/ret_slots，
    // 然后要么回到 loop_check，要么直接返回给 wrapper。
    match template {
        RuntimeHandlerTemplate::MovImm => {
            let dst = operands.get("dst")?;
            let imm = operands.get("imm")?;
            let width = operands.get("width")?;
            let value = mask_to_width(builder, ctx.i64_type, imm, width)?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::ConstLoad => {
            let dst = operands.get("dst")?;
            let index = operands.get("index")?;
            let width = operands.get("width")?;
            let value = read_const_pool_value(builder, ctx, index)?;
            let value = mask_to_width(builder, ctx.i64_type, value, width)?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::SuperAddXor => {
            emit_super_add_xor_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::SuperIcmpBrIf => {
            emit_super_icmp_br_if_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::SuperGepLoad => {
            emit_super_gep_load_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::SuperLoadAdd => {
            emit_super_load_add_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Mov => {
            let dst = operands.get("dst")?;
            let src = operands.get("src")?;
            let width = operands.get("width")?;
            let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "mov.src")?;
            let value = mask_to_width(builder, ctx.i64_type, value, width)?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::Bin(op) => {
            emit_bin_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::IntUnary(op) => {
            emit_int_unary_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::IntTernary(op) => {
            emit_int_ternary_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::FloatBin(op) => {
            emit_float_bin_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::FloatUnary(op) => {
            emit_float_unary_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::FloatCast(op) => {
            emit_float_cast_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::Icmp => {
            emit_icmp_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Fcmp => {
            emit_fcmp_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Cast(op) => {
            emit_cast_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::Alloca => {
            emit_alloca_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Load => {
            emit_load_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Store => {
            emit_store_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::AtomicLoad => {
            emit_atomic_load_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::AtomicStore => {
            emit_atomic_store_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::AtomicRmw(op) => {
            emit_atomic_rmw_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::CmpXchg => {
            emit_cmpxchg_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Fence => {
            emit_fence_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Gep => {
            let dst = operands.get("dst")?;
            let base = operands.get("base")?;
            let offset = operands.get("offset")?;
            let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, base, "gep.base.addr")?;
            let addr = builder.build_int_add(base_addr, offset, "gep.addr")?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, addr)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::CallNative => {
            emit_call_native_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Nop => {
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::Br => {
            let target = operands.get("target")?;
            builder.build_store(ctx.pc_ptr, target)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::BrCond => {
            let cond = operands.get("cond")?;
            let then_pc = operands.get("then_pc")?;
            let else_pc = operands.get("else_pc")?;
            let cond_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, cond, "cond.value")?;
            let is_true =
                builder.build_int_compare(IntPredicate::NE, cond_value, ctx.i64_type.const_zero(), "cond.true")?;
            let selected = builder
                .build_select(is_true, then_pc, else_pc, "next.pc")?
                .into_int_value();
            builder.build_store(ctx.pc_ptr, selected)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::VmCall => {
            let target = operands.get("target")?;
            let pc = load_i64(builder, ctx.i64_type, ctx.pc_ptr, "vm.call.pc")?;
            let return_pc = builder.build_int_add(
                pc,
                ctx.i64_type.const_int(ctx.decoded_width as u64, false),
                "vm.call.return.pc",
            )?;
            store_reg(
                builder,
                ctx.i64_type,
                ctx.x_type,
                ctx.regs,
                ctx.i64_type.const_int(ctx.lr_register as u64, false),
                return_pc,
            )?;
            builder.build_store(ctx.pc_ptr, target)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::VmRet => {
            let return_pc = load_reg(
                builder,
                ctx.i64_type,
                ctx.x_type,
                ctx.regs,
                ctx.i64_type.const_int(ctx.lr_register as u64, false),
                "vm.ret.pc",
            )?;
            builder.build_store(ctx.pc_ptr, return_pc)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::Ret => {
            let src = operands.get("src")?;
            let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "ret.value")?;
            store_reg(
                builder,
                ctx.i64_type,
                ctx.x_type,
                ctx.regs,
                ctx.i64_type.const_int(ctx.return_reg as u64, false),
                value,
            )?;
            store_return_slots(builder, ctx)?;
            builder.build_return(Some(&value))?;
        },
    }

    Ok(())
}

fn has_assign_reg(statements: &[SemanticStmt], dst: &str, expected: &SemanticExpr) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignReg { dst: actual, value } if actual == dst && value == expected
        )
    })
}

fn add_xor_template(statements: &[SemanticStmt]) -> bool {
    has_assign_reg(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::Xor,
                lhs: Box::new(SemanticExpr::Binary {
                    op: SemanticBinOp::Add,
                    lhs: Box::new(reg("lhs")),
                    rhs: Box::new(reg("rhs")),
                }),
                rhs: Box::new(reg("xor_rhs")),
            },
            operand("width"),
        ),
    )
}

fn icmp_br_if_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Select {
                    cond,
                    then_pc,
                    else_pc,
                }
            } if cond.as_ref() == &compare_expr() && then_pc == "then_pc" && else_pc == "else_pc"
        )
    })
}

fn gep_load_template(statements: &[SemanticStmt]) -> bool {
    has_assign_reg(
        statements,
        "dst",
        &SemanticExpr::LoadWidth {
            ptr: Box::new(gep_expr()),
            width: Box::new(operand("width")),
        },
    ) && pc_next(statements)
}

fn load_add_template(statements: &[SemanticStmt]) -> bool {
    has_assign_reg(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::Add,
                lhs: Box::new(SemanticExpr::LoadWidth {
                    ptr: Box::new(reg("ptr")),
                    width: Box::new(operand("width")),
                }),
                rhs: Box::new(reg("addend")),
            },
            operand("width"),
        ),
    ) && pc_next(statements)
}

fn bin_template(statements: &[SemanticStmt]) -> Option<BinOp> {
    [
        (SemanticBinOp::Add, BinOp::Add),
        (SemanticBinOp::Sub, BinOp::Sub),
        (SemanticBinOp::Mul, BinOp::Mul),
        (SemanticBinOp::UDiv, BinOp::UDiv),
        (SemanticBinOp::SDiv, BinOp::SDiv),
        (SemanticBinOp::URem, BinOp::URem),
        (SemanticBinOp::SRem, BinOp::SRem),
        (SemanticBinOp::Xor, BinOp::Xor),
        (SemanticBinOp::And, BinOp::And),
        (SemanticBinOp::Or, BinOp::Or),
        (SemanticBinOp::Shl, BinOp::Shl),
        (SemanticBinOp::LShr, BinOp::LShr),
    ]
    .into_iter()
    .find_map(|(semantic_op, bin_op)| {
        has_assign_reg(
            statements,
            "dst",
            &trunc_width(
                SemanticExpr::Binary {
                    op: semantic_op,
                    lhs: Box::new(reg("lhs")),
                    rhs: Box::new(reg("rhs")),
                },
                operand("width"),
            ),
        )
        .then_some(bin_op)
    })
}

fn int_unary_template(statements: &[SemanticStmt]) -> Option<IntUnaryOp> {
    [
        (SemanticIntUnaryOp::CtPop, IntUnaryOp::CtPop),
        (SemanticIntUnaryOp::BSwap, IntUnaryOp::BSwap),
        (SemanticIntUnaryOp::BitReverse, IntUnaryOp::BitReverse),
    ]
    .into_iter()
    .find_map(|(semantic_op, runtime_op)| {
        has_assign_reg(
            statements,
            "dst",
            &SemanticExpr::IntUnary {
                op: semantic_op,
                value: Box::new(reg("src")),
                width: Box::new(operand("width")),
            },
        )
        .then_some(runtime_op)
    })
    .filter(|_| pc_next(statements))
}

fn int_ternary_template(statements: &[SemanticStmt]) -> Option<IntTernaryOp> {
    [
        (SemanticIntTernaryOp::FShl, IntTernaryOp::FShl),
        (SemanticIntTernaryOp::FShr, IntTernaryOp::FShr),
    ]
    .into_iter()
    .find_map(|(semantic_op, runtime_op)| {
        has_assign_reg(
            statements,
            "dst",
            &SemanticExpr::IntTernary {
                op: semantic_op,
                lhs: Box::new(reg("lhs")),
                rhs: Box::new(reg("rhs")),
                third: Box::new(reg("third")),
                width: Box::new(operand("width")),
            },
        )
        .then_some(runtime_op)
    })
    .filter(|_| pc_next(statements))
}

fn float_bin_template(statements: &[SemanticStmt]) -> Option<FloatBinOp> {
    [
        (SemanticFloatBinOp::Add, FloatBinOp::Add),
        (SemanticFloatBinOp::Sub, FloatBinOp::Sub),
        (SemanticFloatBinOp::Mul, FloatBinOp::Mul),
        (SemanticFloatBinOp::Div, FloatBinOp::Div),
        (SemanticFloatBinOp::Rem, FloatBinOp::Rem),
    ]
    .into_iter()
    .find_map(|(semantic_op, bin_op)| {
        has_assign_reg(
            statements,
            "dst",
            &SemanticExpr::FloatBinary {
                op: semantic_op,
                lhs: Box::new(reg("lhs")),
                rhs: Box::new(reg("rhs")),
                width: Box::new(operand("width")),
            },
        )
        .then_some(bin_op)
    })
}

fn ashr_template(statements: &[SemanticStmt]) -> bool {
    has_assign_reg(
        statements,
        "dst",
        &trunc_width(
            SemanticExpr::Binary {
                op: SemanticBinOp::AShr,
                lhs: Box::new(SemanticExpr::SignExtend {
                    value: Box::new(reg("lhs")),
                    from_width: Box::new(operand("width")),
                    to_width: None,
                }),
                rhs: Box::new(reg("rhs")),
            },
            operand("width"),
        ),
    )
}

fn store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::StoreWidth { ptr, value, width }
                if ptr == &reg("ptr") && value == &reg("src") && width == &operand("width")
        )
    })
}

fn atomic_store_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AtomicStoreWidth {
                ptr,
                value,
                width,
                ordering,
            } if ptr == &reg("ptr")
                && value == &reg("src")
                && width == &operand("width")
                && ordering == &operand("ordering")
        )
    })
}

fn atomic_rmw_template(statements: &[SemanticStmt]) -> Option<AtomicRmwOp> {
    [
        (SemanticAtomicRmwOp::Xchg, AtomicRmwOp::Xchg),
        (SemanticAtomicRmwOp::Add, AtomicRmwOp::Add),
        (SemanticAtomicRmwOp::Sub, AtomicRmwOp::Sub),
        (SemanticAtomicRmwOp::And, AtomicRmwOp::And),
        (SemanticAtomicRmwOp::Or, AtomicRmwOp::Or),
        (SemanticAtomicRmwOp::Xor, AtomicRmwOp::Xor),
        (SemanticAtomicRmwOp::Nand, AtomicRmwOp::Nand),
        (SemanticAtomicRmwOp::Max, AtomicRmwOp::Max),
        (SemanticAtomicRmwOp::Min, AtomicRmwOp::Min),
        (SemanticAtomicRmwOp::UMax, AtomicRmwOp::UMax),
        (SemanticAtomicRmwOp::UMin, AtomicRmwOp::UMin),
    ]
    .into_iter()
    .find_map(|(semantic_op, runtime_op)| {
        has_assign_reg(
            statements,
            "dst",
            &SemanticExpr::AtomicRmw {
                op: semantic_op,
                ptr: Box::new(reg("ptr")),
                value: Box::new(reg("src")),
                width: Box::new(operand("width")),
                ordering: Box::new(operand("ordering")),
            },
        )
        .then_some(runtime_op)
    })
    .filter(|_| pc_next(statements))
}

fn cmpxchg_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::CmpXchg {
                old,
                success,
                ptr,
                compare,
                new,
                width,
                success_ordering,
                failure_ordering,
            } if old == "old"
                && success == "success"
                && ptr == &reg("ptr")
                && compare == &reg("cmp")
                && new == &reg("new")
                && width == &operand("width")
                && success_ordering == &operand("success_ordering")
                && failure_ordering == &operand("failure_ordering")
        )
    }) && pc_next(statements)
}

fn fence_template(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::Fence { ordering } if ordering == &operand("ordering")))
        && pc_next(statements)
}

fn call_native_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignReg {
                value: SemanticExpr::CallTableReturn { callee, .. },
                ..
            } if callee == "callee"
        )
    })
}

fn pc_label(statements: &[SemanticStmt], label: &str) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Label(actual)
            } if actual == label
        )
    })
}

fn pc_register(statements: &[SemanticStmt], register: &str) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Register(actual)
            } if actual == register
        )
    })
}

fn pc_return(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::AssignPc { value: PcExpr::Return }))
}

fn pc_next(statements: &[SemanticStmt]) -> bool {
    statements
        .iter()
        .any(|stmt| matches!(stmt, SemanticStmt::AssignPc { value: PcExpr::Next }))
}

fn pc_select_template(statements: &[SemanticStmt]) -> bool {
    statements.iter().any(|stmt| {
        matches!(
            stmt,
            SemanticStmt::AssignPc {
                value: PcExpr::Select {
                    cond,
                    then_pc,
                    else_pc,
                }
            } if cond.as_ref() == &reg("cond") && then_pc == "then_pc" && else_pc == "else_pc"
        )
    })
}

fn operand(name: &str) -> SemanticExpr {
    SemanticExpr::Operand(name.to_owned())
}

fn reg(name: &str) -> SemanticExpr {
    SemanticExpr::Register(name.to_owned())
}

fn trunc_width(value: SemanticExpr, width: SemanticExpr) -> SemanticExpr {
    SemanticExpr::TruncWidth {
        value: Box::new(value),
        width: Box::new(width),
    }
}

fn float_unary_template(statements: &[SemanticStmt]) -> Option<FloatUnaryOp> {
    let templates = [(SemanticFloatUnaryOp::Neg, FloatUnaryOp::Neg)];
    templates
        .iter()
        .find_map(|(semantic_op, runtime_op)| {
            has_assign_reg(
                statements,
                "dst",
                &SemanticExpr::FloatUnary {
                    op: *semantic_op,
                    value: Box::new(reg("src")),
                    width: Box::new(operand("width")),
                },
            )
            .then_some(*runtime_op)
        })
        .filter(|_| pc_next(statements))
}

fn float_cast_template(statements: &[SemanticStmt]) -> Option<FloatCastOp> {
    [
        (SemanticFloatCastOp::SignedIntToFloat, FloatCastOp::SignedIntToFloat),
        (SemanticFloatCastOp::UnsignedIntToFloat, FloatCastOp::UnsignedIntToFloat),
        (SemanticFloatCastOp::FloatToSignedInt, FloatCastOp::FloatToSignedInt),
        (SemanticFloatCastOp::FloatToUnsignedInt, FloatCastOp::FloatToUnsignedInt),
        (SemanticFloatCastOp::FloatTrunc, FloatCastOp::FloatTrunc),
        (SemanticFloatCastOp::FloatExt, FloatCastOp::FloatExt),
    ]
    .into_iter()
    .find_map(|(semantic_op, runtime_op)| {
        has_assign_reg(
            statements,
            "dst",
            &SemanticExpr::FloatCast {
                op: semantic_op,
                value: Box::new(reg("src")),
                from_width: Box::new(operand("from_width")),
                to_width: Box::new(operand("to_width")),
            },
        )
        .then_some(runtime_op)
    })
    .filter(|_| pc_next(statements))
}

fn compare_expr() -> SemanticExpr {
    SemanticExpr::Compare {
        pred: Box::new(operand("pred")),
        lhs: Box::new(reg("lhs")),
        rhs: Box::new(reg("rhs")),
        width: Box::new(operand("width")),
    }
}

fn float_compare_expr() -> SemanticExpr {
    SemanticExpr::FloatCompare {
        pred: Box::new(operand("pred")),
        lhs: Box::new(reg("lhs")),
        rhs: Box::new(reg("rhs")),
        width: Box::new(operand("width")),
    }
}

fn zero_extend_expr() -> SemanticExpr {
    SemanticExpr::ZeroExtend {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Box::new(operand("to_width")),
    }
}

fn sign_extend_expr() -> SemanticExpr {
    SemanticExpr::SignExtend {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Some(Box::new(operand("to_width"))),
    }
}

fn bitcast_expr() -> SemanticExpr {
    SemanticExpr::BitcastWidth {
        value: Box::new(reg("src")),
        from_width: Box::new(operand("from_width")),
        to_width: Box::new(operand("to_width")),
    }
}

fn stack_alloc_expr() -> SemanticExpr {
    SemanticExpr::StackAlloc {
        bytes: Box::new(operand("bytes")),
        align: Box::new(operand("align")),
    }
}

fn load_width_expr() -> SemanticExpr {
    SemanticExpr::LoadWidth {
        ptr: Box::new(reg("ptr")),
        width: Box::new(operand("width")),
    }
}

fn atomic_load_width_expr() -> SemanticExpr {
    SemanticExpr::AtomicLoadWidth {
        ptr: Box::new(reg("ptr")),
        width: Box::new(operand("width")),
        ordering: Box::new(operand("ordering")),
    }
}

fn gep_expr() -> SemanticExpr {
    SemanticExpr::Binary {
        op: SemanticBinOp::Add,
        lhs: Box::new(reg("base")),
        rhs: Box::new(operand("offset")),
    }
}

fn handler_order(profile: &ProfilePackage, salt: &str) -> Vec<usize> {
    let mut order = (0..profile.isa.instructions.len()).collect::<Vec<_>>();
    if profile.runtime.enhancements.handler_order_shuffle {
        order.sort_by_key(|index| {
            let mut hasher = DefaultHasher::new();
            salt.hash(&mut hasher);
            profile.isa.instructions[*index].name.hash(&mut hasher);
            profile.isa.instructions[*index].opcode.hash(&mut hasher);
            hasher.finish()
        });
    }
    order
}

fn handler_alias_order(profile: &ProfilePackage, salt: &str) -> Vec<(usize, Opcode)> {
    let mut order = handler_order(profile, salt)
        .into_iter()
        .flat_map(|instruction_index| {
            profile.isa.instructions[instruction_index]
                .opcodes()
                .iter()
                .copied()
                .map(move |opcode| (instruction_index, opcode))
        })
        .collect::<Vec<_>>();
    if profile.runtime.enhancements.handler_order_shuffle {
        order.sort_by_key(|(instruction_index, opcode)| {
            let mut hasher = DefaultHasher::new();
            salt.hash(&mut hasher);
            profile.isa.instructions[*instruction_index].name.hash(&mut hasher);
            opcode.hash(&mut hasher);
            hasher.finish()
        });
    }
    order
}

fn alias_x_register(profile: &ProfilePackage, alias: &str) -> anyhow::Result<u8> {
    let register = profile
        .runtime
        .aliases
        .get(alias)
        .ok_or_else(|| anyhow::anyhow!("runtime.vm does not define register alias {alias}"))?;
    let index = register
        .strip_prefix('x')
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| anyhow::anyhow!("runtime alias {alias} must point to an x register, got {register}"))?;
    if index >= 32 {
        anyhow::bail!("runtime alias {alias} points outside x0..x31: {register}");
    }
    Ok(index)
}

fn emit_alloca_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let bytes = operands.get("bytes")?;
    let _align = operands.get("align")?;
    let ptr = builder.build_array_alloca(ctx.i8_type, bytes, "vm.alloca")?;
    let addr = builder.build_ptr_to_int(ptr, ctx.i64_type, "vm.alloca.addr")?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, addr)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_load_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let ptr_reg = operands.get("ptr")?;
    let width = operands.get("width")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr_reg, "load.base")?;
    emit_scalar_load_from_address(builder, ctx, dst, base_addr, width, "load")
}

fn emit_super_gep_load_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let base = operands.get("base")?;
    let offset = operands.get("offset")?;
    let width = operands.get("width")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, base, "gep_load.base")?;
    let addr = builder.build_int_add(base_addr, offset, "gep_load.addr")?;
    emit_scalar_load_from_address(builder, ctx, dst, addr, width, "gep_load")
}

fn emit_super_load_add_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let ptr = operands.get("ptr")?;
    let addend = operands.get("addend")?;
    let width = operands.get("width")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr, "load_iadd.ptr")?;
    let loaded = read_scalar_load_from_address(builder, ctx, base_addr, width, "load_iadd")?;
    let addend_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, addend, "load_iadd.addend")?;
    let raw = builder.build_int_add(loaded, addend_value, "load_iadd.add")?;
    let value = mask_to_width(builder, ctx.i64_type, raw, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_scalar_load_from_address<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    dst: IntValue<'ctx>,
    base_addr: IntValue<'ctx>,
    width: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<()> {
    let result = read_scalar_load_from_address(builder, ctx, base_addr, width, name)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, result)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn read_scalar_load_from_address<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    base_addr: IntValue<'ctx>,
    width: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let byte_count = width_to_byte_count(builder, ctx.i64_type, width)?;

    let index_ptr = builder.build_alloca(ctx.i64_type, &format!("{name}.index"))?;
    let result_ptr = builder.build_alloca(ctx.i64_type, &format!("{name}.result"))?;
    builder.build_store(index_ptr, ctx.i64_type.const_zero())?;
    builder.build_store(result_ptr, ctx.i64_type.const_zero())?;

    let loop_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, &format!("{name}.loop"));
    let body_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, &format!("{name}.body"));
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, &format!("{name}.done"));
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(loop_block);
    let index = load_i64(builder, ctx.i64_type, index_ptr, &format!("{name}.index.cur"))?;
    let in_range = builder.build_int_compare(IntPredicate::ULT, index, byte_count, &format!("{name}.in.range"))?;
    builder.build_conditional_branch(in_range, body_block, done_block)?;

    builder.position_at_end(body_block);
    let byte_addr = builder.build_int_add(base_addr, index, &format!("{name}.byte.addr"))?;
    let byte_ptr = builder.build_int_to_ptr(byte_addr, ctx.ptr_type, &format!("{name}.byte.ptr"))?;
    let byte = builder
        .build_load2(ctx.i8_type, byte_ptr, &format!("{name}.byte"))?
        .into_int_value();
    let byte64 = builder.build_int_z_extend(byte, ctx.i64_type, &format!("{name}.byte64"))?;
    let shift = builder.build_int_mul(index, ctx.i64_type.const_int(8, false), &format!("{name}.shift"))?;
    let shifted = builder.build_left_shift(byte64, shift, &format!("{name}.shifted"))?;
    let old_result = load_i64(builder, ctx.i64_type, result_ptr, &format!("{name}.result.old"))?;
    let new_result = builder.build_or(old_result, shifted, &format!("{name}.result.new"))?;
    builder.build_store(result_ptr, new_result)?;
    let next_index = builder.build_int_add(index, ctx.i64_type.const_int(1, false), &format!("{name}.index.next"))?;
    builder.build_store(index_ptr, next_index)?;
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(builder, ctx.i64_type, result_ptr, &format!("{name}.result.final"))?;
    mask_to_width(builder, ctx.i64_type, result, width)
}

fn emit_store_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let src = operands.get("src")?;
    let ptr_reg = operands.get("ptr")?;
    let width = operands.get("width")?;
    let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "store.value")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr_reg, "store.base")?;
    let byte_count = width_to_byte_count(builder, ctx.i64_type, width)?;

    let index_ptr = builder.build_alloca(ctx.i64_type, "store.index")?;
    builder.build_store(index_ptr, ctx.i64_type.const_zero())?;

    let loop_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "store.loop");
    let body_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "store.body");
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "store.done");
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(loop_block);
    let index = load_i64(builder, ctx.i64_type, index_ptr, "store.index.cur")?;
    let in_range = builder.build_int_compare(IntPredicate::ULT, index, byte_count, "store.in.range")?;
    builder.build_conditional_branch(in_range, body_block, done_block)?;

    builder.position_at_end(body_block);
    let shift = builder.build_int_mul(index, ctx.i64_type.const_int(8, false), "store.shift")?;
    let shifted = builder.build_right_shift(value, shift, false, "store.shifted")?;
    let byte = builder.build_int_truncate(shifted, ctx.i8_type, "store.byte")?;
    let byte_addr = builder.build_int_add(base_addr, index, "store.byte.addr")?;
    let byte_ptr = builder.build_int_to_ptr(byte_addr, ctx.ptr_type, "store.byte.ptr")?;
    builder.build_store(byte_ptr, byte)?;
    let next_index = builder.build_int_add(index, ctx.i64_type.const_int(1, false), "store.index.next")?;
    builder.build_store(index_ptr, next_index)?;
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_atomic_load_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let ptr_reg = operands.get("ptr")?;
    let width = operands.get("width")?;
    let ordering = operands.get("ordering")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr_reg, "atomic.load.base")?;
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "atomic.load.done");

    for (width_bits, llvm_ordering) in atomic_load_cases() {
        let case_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.load.case");
        let next_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.load.next");
        let width_match = builder.build_int_compare(
            IntPredicate::EQ,
            width,
            ctx.i64_type.const_int(width_bits, false),
            "atomic.load.width.match",
        )?;
        let ordering_match = builder.build_int_compare(
            IntPredicate::EQ,
            ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(llvm_ordering), false),
            "atomic.load.order.match",
        )?;
        let matched = builder.build_and(width_match, ordering_match, "atomic.load.match")?;
        builder.build_conditional_branch(matched, case_block, next_block)?;

        builder.position_at_end(case_block);
        emit_atomic_load_case(builder, ctx, dst, base_addr, width_bits, llvm_ordering)?;
        builder.build_unconditional_branch(done_block)?;
        builder.position_at_end(next_block);
    }

    builder.build_unconditional_branch(done_block)?;
    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_atomic_store_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let src = operands.get("src")?;
    let ptr_reg = operands.get("ptr")?;
    let width = operands.get("width")?;
    let ordering = operands.get("ordering")?;
    let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "atomic.store.value")?;
    let base_addr = load_reg(
        builder,
        ctx.i64_type,
        ctx.x_type,
        ctx.regs,
        ptr_reg,
        "atomic.store.base",
    )?;
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "atomic.store.done");

    for (width_bits, llvm_ordering) in atomic_store_cases() {
        let case_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.store.case");
        let next_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.store.next");
        let width_match = builder.build_int_compare(
            IntPredicate::EQ,
            width,
            ctx.i64_type.const_int(width_bits, false),
            "atomic.store.width.match",
        )?;
        let ordering_match = builder.build_int_compare(
            IntPredicate::EQ,
            ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(llvm_ordering), false),
            "atomic.store.order.match",
        )?;
        let matched = builder.build_and(width_match, ordering_match, "atomic.store.match")?;
        builder.build_conditional_branch(matched, case_block, next_block)?;

        builder.position_at_end(case_block);
        emit_atomic_store_case(builder, ctx, value, base_addr, width_bits, llvm_ordering)?;
        builder.build_unconditional_branch(done_block)?;
        builder.position_at_end(next_block);
    }

    builder.build_unconditional_branch(done_block)?;
    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_atomic_load_case<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    dst: IntValue<'ctx>,
    base_addr: IntValue<'ctx>,
    width_bits: u64,
    ordering: AtomicOrdering,
) -> anyhow::Result<()> {
    let int_type = int_type_for_width(ctx, width_bits)?;
    let ptr = builder.build_int_to_ptr(base_addr, ctx.ptr_type, "atomic.load.ptr")?;
    let raw = builder
        .build_load2(int_type, ptr, "atomic.load.value")?
        .into_int_value();
    let load_inst = raw
        .as_instruction_value()
        .context("atomic load should produce an instruction")?;
    load_inst.set_atomic_ordering(ordering)?;
    load_inst.set_alignment(atomic_alignment(width_bits)?)?;
    let value = if width_bits == 64 {
        raw
    } else {
        builder.build_int_z_extend(raw, ctx.i64_type, "atomic.load.zext")?
    };
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    Ok(())
}

fn emit_atomic_store_case<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    value: IntValue<'ctx>,
    base_addr: IntValue<'ctx>,
    width_bits: u64,
    ordering: AtomicOrdering,
) -> anyhow::Result<()> {
    let int_type = int_type_for_width(ctx, width_bits)?;
    let ptr = builder.build_int_to_ptr(base_addr, ctx.ptr_type, "atomic.store.ptr")?;
    let stored = if width_bits == 64 {
        value
    } else {
        builder.build_int_truncate(value, int_type, "atomic.store.trunc")?
    };
    let store_inst = builder.build_store(ptr, stored)?;
    store_inst.set_atomic_ordering(ordering)?;
    store_inst.set_alignment(atomic_alignment(width_bits)?)?;
    Ok(())
}

fn emit_atomic_rmw_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: AtomicRmwOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let ptr_reg = operands.get("ptr")?;
    let src = operands.get("src")?;
    let width = operands.get("width")?;
    let ordering = operands.get("ordering")?;
    let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "atomic.rmw.value")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr_reg, "atomic.rmw.base")?;
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "atomic.rmw.done");

    for (width_bits, llvm_ordering) in atomic_rmw_cases() {
        let case_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.rmw.case");
        let next_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "atomic.rmw.next");
        let width_match = builder.build_int_compare(
            IntPredicate::EQ,
            width,
            ctx.i64_type.const_int(width_bits, false),
            "atomic.rmw.width.match",
        )?;
        let ordering_match = builder.build_int_compare(
            IntPredicate::EQ,
            ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(llvm_ordering), false),
            "atomic.rmw.order.match",
        )?;
        let matched = builder.build_and(width_match, ordering_match, "atomic.rmw.match")?;
        builder.build_conditional_branch(matched, case_block, next_block)?;

        builder.position_at_end(case_block);
        emit_atomic_rmw_case(builder, ctx, op, dst, value, base_addr, width_bits, llvm_ordering)?;
        builder.build_unconditional_branch(done_block)?;
        builder.position_at_end(next_block);
    }

    builder.build_unconditional_branch(done_block)?;
    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_atomic_rmw_case<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: AtomicRmwOp,
    dst: IntValue<'ctx>,
    value: IntValue<'ctx>,
    base_addr: IntValue<'ctx>,
    width_bits: u64,
    ordering: AtomicOrdering,
) -> anyhow::Result<()> {
    let int_type = int_type_for_width(ctx, width_bits)?;
    let ptr = builder.build_int_to_ptr(base_addr, ctx.ptr_type, "atomic.rmw.ptr")?;
    let operand = if width_bits == 64 {
        value
    } else {
        builder.build_int_truncate(value, int_type, "atomic.rmw.trunc")?
    };
    let old = builder.build_atomicrmw(atomic_rmw_op_for_llvm(op), ptr, operand, ordering)?;
    let old_inst = old
        .as_instruction_value()
        .context("atomicrmw should produce an instruction")?;
    let alignment = atomic_alignment(width_bits)?;
    // SAFETY: `old_inst` 是刚由 LLVMBuildAtomicRMW 创建的 live instruction；这里仅写入
    // alignment metadata，且 alignment 来自已限制的 8/16/32/64 位自然对齐。
    unsafe { LLVMSetAlignment(old_inst.as_value_ref(), alignment) };
    let old = if width_bits == 64 {
        old
    } else {
        builder.build_int_z_extend(old, ctx.i64_type, "atomic.rmw.old.zext")?
    };
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, old)?;
    Ok(())
}

fn emit_cmpxchg_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let old_dst = operands.get("old")?;
    let success_dst = operands.get("success")?;
    let ptr_reg = operands.get("ptr")?;
    let cmp_reg = operands.get("cmp")?;
    let new_reg = operands.get("new")?;
    let width = operands.get("width")?;
    let success_ordering = operands.get("success_ordering")?;
    let failure_ordering = operands.get("failure_ordering")?;
    let cmp = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, cmp_reg, "cmpxchg.cmp")?;
    let new = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, new_reg, "cmpxchg.new")?;
    let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, ptr_reg, "cmpxchg.base")?;
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "cmpxchg.done");

    for (width_bits, success_order, failure_order) in cmpxchg_cases() {
        let case_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "cmpxchg.case");
        let next_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "cmpxchg.next");
        let width_match = builder.build_int_compare(
            IntPredicate::EQ,
            width,
            ctx.i64_type.const_int(width_bits, false),
            "cmpxchg.width.match",
        )?;
        let success_match = builder.build_int_compare(
            IntPredicate::EQ,
            success_ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(success_order), false),
            "cmpxchg.success.order.match",
        )?;
        let failure_match = builder.build_int_compare(
            IntPredicate::EQ,
            failure_ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(failure_order), false),
            "cmpxchg.failure.order.match",
        )?;
        let order_match = builder.build_and(success_match, failure_match, "cmpxchg.order.match")?;
        let matched = builder.build_and(width_match, order_match, "cmpxchg.match")?;
        builder.build_conditional_branch(matched, case_block, next_block)?;

        builder.position_at_end(case_block);
        emit_cmpxchg_case(
            builder,
            ctx,
            old_dst,
            success_dst,
            cmp,
            new,
            base_addr,
            width_bits,
            success_order,
            failure_order,
        )?;
        builder.build_unconditional_branch(done_block)?;
        builder.position_at_end(next_block);
    }

    builder.build_unconditional_branch(done_block)?;
    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_cmpxchg_case<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    old_dst: IntValue<'ctx>,
    success_dst: IntValue<'ctx>,
    cmp: IntValue<'ctx>,
    new: IntValue<'ctx>,
    base_addr: IntValue<'ctx>,
    width_bits: u64,
    success_ordering: AtomicOrdering,
    failure_ordering: AtomicOrdering,
) -> anyhow::Result<()> {
    let int_type = int_type_for_width(ctx, width_bits)?;
    let ptr = builder.build_int_to_ptr(base_addr, ctx.ptr_type, "cmpxchg.ptr")?;
    let cmp = if width_bits == 64 {
        cmp
    } else {
        builder.build_int_truncate(cmp, int_type, "cmpxchg.cmp.trunc")?
    };
    let new = if width_bits == 64 {
        new
    } else {
        builder.build_int_truncate(new, int_type, "cmpxchg.new.trunc")?
    };
    let pair = builder.build_cmpxchg(ptr, cmp, new, success_ordering, failure_ordering)?;
    let alignment = atomic_alignment(width_bits)?;
    // SAFETY: `pair` 是刚由 LLVMBuildAtomicCmpXchg 创建的 live instruction；这里仅写入
    // alignment metadata，且 alignment 来自已限制的 8/16/32/64 位自然对齐。
    unsafe { LLVMSetAlignment(pair.as_value_ref(), alignment) };
    let old = builder.build_extract_value(pair, 0, "cmpxchg.old")?.into_int_value();
    let old = if width_bits == 64 {
        old
    } else {
        builder.build_int_z_extend(old, ctx.i64_type, "cmpxchg.old.zext")?
    };
    let success = builder
        .build_extract_value(pair, 1, "cmpxchg.success")?
        .into_int_value();
    let success = builder.build_int_z_extend(success, ctx.i64_type, "cmpxchg.success.zext")?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, old_dst, old)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, success_dst, success)?;
    Ok(())
}

fn emit_fence_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let ordering = operands.get("ordering")?;
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fence.done");

    for fence_ordering in fence_cases() {
        let case_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "fence.case");
        let next_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, "fence.next");
        let matched = builder.build_int_compare(
            IntPredicate::EQ,
            ordering,
            ctx.i64_type
                .const_int(memory_ordering_tag_for_llvm(fence_ordering), false),
            "fence.order.match",
        )?;
        builder.build_conditional_branch(matched, case_block, next_block)?;

        builder.position_at_end(case_block);
        emit_fence_case(builder, fence_ordering);
        builder.build_unconditional_branch(done_block)?;
        builder.position_at_end(next_block);
    }

    builder.build_unconditional_branch(done_block)?;
    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_fence_case(builder: &amice_plugin::inkwell::builder::Builder<'_>, ordering: AtomicOrdering) {
    // SAFETY: `builder` belongs to the live dispatcher function and is positioned in
    // a handler case block. `ordering` comes from the finite `fence_cases()` set, so
    // LLVM receives only acquire/release/acq_rel/seq_cst fence orderings.
    unsafe {
        LLVMBuildFence(builder.as_mut_ptr(), ordering.into(), 0, c"".as_ptr());
    }
}

fn atomic_load_cases() -> impl Iterator<Item = (u64, AtomicOrdering)> {
    [8, 16, 32, 64].into_iter().flat_map(|width| {
        [
            AtomicOrdering::Unordered,
            AtomicOrdering::Monotonic,
            AtomicOrdering::Acquire,
            AtomicOrdering::SequentiallyConsistent,
        ]
        .into_iter()
        .map(move |ordering| (width, ordering))
    })
}

fn atomic_store_cases() -> impl Iterator<Item = (u64, AtomicOrdering)> {
    [8, 16, 32, 64].into_iter().flat_map(|width| {
        [
            AtomicOrdering::Unordered,
            AtomicOrdering::Monotonic,
            AtomicOrdering::Release,
            AtomicOrdering::SequentiallyConsistent,
        ]
        .into_iter()
        .map(move |ordering| (width, ordering))
    })
}

fn atomic_rmw_cases() -> impl Iterator<Item = (u64, AtomicOrdering)> {
    [8, 16, 32, 64].into_iter().flat_map(|width| {
        [
            AtomicOrdering::Monotonic,
            AtomicOrdering::Acquire,
            AtomicOrdering::Release,
            AtomicOrdering::AcquireRelease,
            AtomicOrdering::SequentiallyConsistent,
        ]
        .into_iter()
        .map(move |ordering| (width, ordering))
    })
}

fn cmpxchg_cases() -> impl Iterator<Item = (u64, AtomicOrdering, AtomicOrdering)> {
    [8, 16, 32, 64].into_iter().flat_map(|width| {
        [
            AtomicOrdering::Monotonic,
            AtomicOrdering::Acquire,
            AtomicOrdering::Release,
            AtomicOrdering::AcquireRelease,
            AtomicOrdering::SequentiallyConsistent,
        ]
        .into_iter()
        .flat_map(move |success| {
            [
                AtomicOrdering::Monotonic,
                AtomicOrdering::Acquire,
                AtomicOrdering::SequentiallyConsistent,
            ]
            .into_iter()
            .filter(move |failure| {
                atomic_ordering_rank_for_cmpxchg(*failure) <= atomic_ordering_rank_for_cmpxchg(success)
            })
            .map(move |failure| (width, success, failure))
        })
    })
}

fn fence_cases() -> impl Iterator<Item = AtomicOrdering> {
    [
        AtomicOrdering::Acquire,
        AtomicOrdering::Release,
        AtomicOrdering::AcquireRelease,
        AtomicOrdering::SequentiallyConsistent,
    ]
    .into_iter()
}

fn atomic_ordering_rank_for_cmpxchg(ordering: AtomicOrdering) -> u8 {
    match ordering {
        AtomicOrdering::Unordered => 1,
        AtomicOrdering::Monotonic => 2,
        AtomicOrdering::Acquire => 3,
        AtomicOrdering::Release => 4,
        AtomicOrdering::AcquireRelease => 5,
        AtomicOrdering::SequentiallyConsistent => 6,
        AtomicOrdering::NotAtomic => 0,
    }
}

fn atomic_rmw_op_for_llvm(op: AtomicRmwOp) -> AtomicRMWBinOp {
    match op {
        AtomicRmwOp::Xchg => AtomicRMWBinOp::Xchg,
        AtomicRmwOp::Add => AtomicRMWBinOp::Add,
        AtomicRmwOp::Sub => AtomicRMWBinOp::Sub,
        AtomicRmwOp::And => AtomicRMWBinOp::And,
        AtomicRmwOp::Or => AtomicRMWBinOp::Or,
        AtomicRmwOp::Xor => AtomicRMWBinOp::Xor,
        AtomicRmwOp::Nand => AtomicRMWBinOp::Nand,
        AtomicRmwOp::Max => AtomicRMWBinOp::Max,
        AtomicRmwOp::Min => AtomicRMWBinOp::Min,
        AtomicRmwOp::UMax => AtomicRMWBinOp::UMax,
        AtomicRmwOp::UMin => AtomicRMWBinOp::UMin,
    }
}

fn memory_ordering_tag_for_llvm(ordering: AtomicOrdering) -> u64 {
    match ordering {
        AtomicOrdering::Unordered => MemoryOrdering::Unordered as u64,
        AtomicOrdering::Monotonic => MemoryOrdering::Monotonic as u64,
        AtomicOrdering::Acquire => MemoryOrdering::Acquire as u64,
        AtomicOrdering::Release => MemoryOrdering::Release as u64,
        AtomicOrdering::AcquireRelease => MemoryOrdering::AcquireRelease as u64,
        AtomicOrdering::SequentiallyConsistent => MemoryOrdering::SequentiallyConsistent as u64,
        AtomicOrdering::NotAtomic => 0,
    }
}

fn int_type_for_width<'ctx>(ctx: HandlerContext<'ctx, '_>, width_bits: u64) -> anyhow::Result<IntType<'ctx>> {
    let llvm_context = ctx.function.get_type().get_context();
    match width_bits {
        8 => Ok(ctx.i8_type),
        16 => Ok(llvm_context.i16_type()),
        32 => Ok(llvm_context.i32_type()),
        64 => Ok(ctx.i64_type),
        _ => anyhow::bail!("atomic handler only supports i8/i16/i32/i64 widths, got {width_bits}"),
    }
}

fn atomic_alignment(width_bits: u64) -> anyhow::Result<u32> {
    match width_bits {
        8 | 16 | 32 | 64 => Ok((width_bits / 8) as u32),
        _ => anyhow::bail!("atomic alignment requires i8/i16/i32/i64 width, got {width_bits}"),
    }
}

fn emit_call_native_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let call_id = operands.get("callee")?;
    let argc = operands.get("argc")?;
    let arg_regs = (0..NATIVE_CALL_MAX_ARGS)
        .map(|index| operands.get(&format!("arg{index}")))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ret_count = operands.get("ret_count")?;
    let ret_slots = (0..NATIVE_CALL_MAX_RETURNS)
        .map(|index| {
            let reg = operands.get(&format!("ret{index}"))?;
            let width = operands.get(&format!("ret{index}_width"))?;
            Ok((reg, width))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let arg_values = arg_regs
        .iter()
        .map(|reg| load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, *reg, "native.arg"))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let call_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "native.call");
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "native.done");
    let argc_ok = builder.build_int_compare(
        IntPredicate::ULE,
        argc,
        ctx.i64_type.const_int(NATIVE_CALL_MAX_ARGS as u64, false),
        "native.argc.ok",
    )?;
    let id_ok = builder.build_int_compare(IntPredicate::ULT, call_id, ctx.native_count, "native.id.ok")?;
    let ret_count_ok = builder.build_int_compare(
        IntPredicate::ULE,
        ret_count,
        ctx.i64_type.const_int(NATIVE_CALL_MAX_RETURNS as u64, false),
        "native.ret.count.ok",
    )?;
    let call_bounds_ok = builder.build_and(argc_ok, id_ok, "native.call.bounds.ok")?;
    let can_call = builder.build_and(call_bounds_ok, ret_count_ok, "native.can.call")?;
    builder.build_conditional_branch(can_call, call_block, done_block)?;

    builder.position_at_end(call_block);
    let slot = builder.build_gep2(ctx.ptr_type, ctx.native_table, &[call_id], "native.slot")?;
    let thunk = builder
        .build_load2(ctx.ptr_type, slot, "native.thunk")?
        .into_pointer_value();
    let thunk_arg_types = (0..NATIVE_CALL_MAX_ARGS)
        .map(|_| ctx.i64_type.into())
        .collect::<Vec<_>>();
    let native_ret_type = ctx
        .function
        .get_type()
        .get_context()
        .struct_type(&vec![ctx.i64_type.into(); NATIVE_CALL_MAX_RETURNS], false);
    let thunk_type = native_ret_type.fn_type(&thunk_arg_types, false);
    let call_args = arg_values
        .iter()
        .map(|value| (*value).into())
        .collect::<Vec<BasicMetadataValueEnum<'ctx>>>();
    let call = builder.build_indirect_call(thunk_type, thunk, &call_args, "native.ret")?;
    let ret_tuple = call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| anyhow::anyhow!("native thunk should return an i64 return tuple"))?
        .into_struct_value();
    let ret_values = ret_slots
        .iter()
        .enumerate()
        .map(|(index, (_, width))| {
            let value = builder
                .build_extract_value(ret_tuple, index as u32, &format!("native.ret{index}.value"))?
                .into_int_value();
            mask_to_width(builder, ctx.i64_type, value, *width)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let store_done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "native.store.done");
    for (index, ((reg, _), value)) in ret_slots.iter().zip(ret_values.iter()).enumerate() {
        let store_block = ctx
            .function
            .get_type()
            .get_context()
            .append_basic_block(ctx.function, &format!("native.ret{index}.store"));
        let next_check_block = (index + 1 < NATIVE_CALL_MAX_RETURNS).then(|| {
            ctx.function
                .get_type()
                .get_context()
                .append_basic_block(ctx.function, &format!("native.ret{}.check", index + 1))
        });
        let has_ret = builder.build_int_compare(
            IntPredicate::UGE,
            ret_count,
            ctx.i64_type.const_int((index + 1) as u64, false),
            &format!("native.has.ret{index}"),
        )?;
        builder.build_conditional_branch(has_ret, store_block, store_done_block)?;

        builder.position_at_end(store_block);
        store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, *reg, *value)?;
        builder.build_unconditional_branch(next_check_block.unwrap_or(store_done_block))?;

        if let Some(next_check_block) = next_check_block {
            builder.position_at_end(next_check_block);
        }
    }

    builder.position_at_end(store_done_block);
    builder.build_unconditional_branch(done_block)?;

    builder.position_at_end(done_block);
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_bin_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: BinOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let width = operands.get("width")?;
    let lhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "lhs")?;
    let rhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "rhs")?;
    let shift = builder.build_and(rhs_value, ctx.i64_type.const_int(63, false), "shift.masked")?;
    let raw = match op {
        BinOp::Add => builder.build_int_add(lhs_value, rhs_value, "bin.add")?,
        BinOp::Sub => builder.build_int_sub(lhs_value, rhs_value, "bin.sub")?,
        BinOp::Mul => builder.build_int_mul(lhs_value, rhs_value, "bin.mul")?,
        BinOp::UDiv => {
            let lhs = mask_to_width(builder, ctx.i64_type, lhs_value, width)?;
            let rhs = mask_to_width(builder, ctx.i64_type, rhs_value, width)?;
            builder.build_int_unsigned_div(lhs, rhs, "bin.udiv")?
        },
        BinOp::SDiv => {
            let lhs = sign_extend_to_i64(builder, ctx.i64_type, lhs_value, width)?;
            let rhs = sign_extend_to_i64(builder, ctx.i64_type, rhs_value, width)?;
            builder.build_int_signed_div(lhs, rhs, "bin.sdiv")?
        },
        BinOp::URem => {
            let lhs = mask_to_width(builder, ctx.i64_type, lhs_value, width)?;
            let rhs = mask_to_width(builder, ctx.i64_type, rhs_value, width)?;
            builder.build_int_unsigned_rem(lhs, rhs, "bin.urem")?
        },
        BinOp::SRem => {
            let lhs = sign_extend_to_i64(builder, ctx.i64_type, lhs_value, width)?;
            let rhs = sign_extend_to_i64(builder, ctx.i64_type, rhs_value, width)?;
            builder.build_int_signed_rem(lhs, rhs, "bin.srem")?
        },
        BinOp::Xor => builder.build_xor(lhs_value, rhs_value, "bin.xor")?,
        BinOp::And => builder.build_and(lhs_value, rhs_value, "bin.and")?,
        BinOp::Or => builder.build_or(lhs_value, rhs_value, "bin.or")?,
        BinOp::Shl => builder.build_left_shift(lhs_value, shift, "bin.shl")?,
        BinOp::LShr => builder.build_right_shift(lhs_value, shift, false, "bin.lshr")?,
        BinOp::AShr => {
            let signed = sign_extend_to_i64(builder, ctx.i64_type, lhs_value, width)?;
            builder.build_right_shift(signed, shift, true, "bin.ashr")?
        },
    };
    let value = mask_to_width(builder, ctx.i64_type, raw, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_super_add_xor_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let xor_rhs = operands.get("xor_rhs")?;
    let width = operands.get("width")?;
    let lhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "super.add.lhs")?;
    let rhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "super.add.rhs")?;
    let xor_rhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, xor_rhs, "super.xor.rhs")?;
    let added = builder.build_int_add(lhs_value, rhs_value, "super.add")?;
    let xored = builder.build_xor(added, xor_rhs_value, "super.xor")?;
    let value = mask_to_width(builder, ctx.i64_type, xored, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_int_unary_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: IntUnaryOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let src = operands.get("src")?;
    let width = operands.get("width")?;
    let src_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "iunary.src")?;
    let masked = mask_to_width(builder, ctx.i64_type, src_value, width)?;
    let raw = match op {
        IntUnaryOp::CtPop => build_popcount_i64(builder, ctx.i64_type, masked)?,
        IntUnaryOp::BSwap => build_bswap_i64(builder, ctx.i64_type, masked, width)?,
        IntUnaryOp::BitReverse => build_bitreverse_i64(builder, ctx.i64_type, masked, width)?,
    };
    let value = mask_to_width(builder, ctx.i64_type, raw, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_int_ternary_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: IntTernaryOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let third = operands.get("third")?;
    let width = operands.get("width")?;
    let lhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "iternary.lhs")?;
    let rhs_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "iternary.rhs")?;
    let third_value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, third, "iternary.third")?;
    let lhs_value = mask_to_width(builder, ctx.i64_type, lhs_value, width)?;
    let rhs_value = mask_to_width(builder, ctx.i64_type, rhs_value, width)?;
    let raw = match op {
        IntTernaryOp::FShl => build_funnel_shift_left(builder, ctx.i64_type, lhs_value, rhs_value, third_value, width)?,
        IntTernaryOp::FShr => {
            build_funnel_shift_right(builder, ctx.i64_type, lhs_value, rhs_value, third_value, width)?
        },
    };
    let value = mask_to_width(builder, ctx.i64_type, raw, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_funnel_shift_left<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
    shift: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let zero = i64_type.const_zero();
    let shift = builder.build_int_unsigned_rem(shift, width, "fshl.shift.mod")?;
    let is_zero = builder.build_int_compare(IntPredicate::EQ, shift, zero, "fshl.shift.zero")?;
    let inverse = builder.build_int_sub(width, shift, "fshl.inverse.raw")?;
    let inverse = builder
        .build_select(is_zero, zero, inverse, "fshl.inverse")?
        .into_int_value();
    let left = builder.build_left_shift(lhs, shift, "fshl.left")?;
    let right = builder.build_right_shift(rhs, inverse, false, "fshl.right")?;
    let combined = builder.build_or(left, right, "fshl.combined")?;
    Ok(builder
        .build_select(is_zero, lhs, combined, "fshl.result")?
        .into_int_value())
}

fn build_funnel_shift_right<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
    shift: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let zero = i64_type.const_zero();
    let shift = builder.build_int_unsigned_rem(shift, width, "fshr.shift.mod")?;
    let is_zero = builder.build_int_compare(IntPredicate::EQ, shift, zero, "fshr.shift.zero")?;
    let inverse = builder.build_int_sub(width, shift, "fshr.inverse.raw")?;
    let inverse = builder
        .build_select(is_zero, zero, inverse, "fshr.inverse")?
        .into_int_value();
    let left = builder.build_left_shift(lhs, inverse, "fshr.left")?;
    let right = builder.build_right_shift(rhs, shift, false, "fshr.right")?;
    let combined = builder.build_or(left, right, "fshr.combined")?;
    Ok(builder
        .build_select(is_zero, rhs, combined, "fshr.result")?
        .into_int_value())
}

fn build_popcount_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let m1 = i64_type.const_int(0x5555_5555_5555_5555, false);
    let m2 = i64_type.const_int(0x3333_3333_3333_3333, false);
    let m4 = i64_type.const_int(0x0f0f_0f0f_0f0f_0f0f, false);
    let h01 = i64_type.const_int(0x0101_0101_0101_0101, false);
    let right1 = builder.build_right_shift(value, i64_type.const_int(1, false), false, "ctpop.shr1")?;
    let paired = builder.build_and(right1, m1, "ctpop.paired")?;
    let value = builder.build_int_sub(value, paired, "ctpop.sub")?;
    let low2 = builder.build_and(value, m2, "ctpop.low2")?;
    let right2 = builder.build_right_shift(value, i64_type.const_int(2, false), false, "ctpop.shr2")?;
    let high2 = builder.build_and(right2, m2, "ctpop.high2")?;
    let value = builder.build_int_add(low2, high2, "ctpop.nibbles")?;
    let right4 = builder.build_right_shift(value, i64_type.const_int(4, false), false, "ctpop.shr4")?;
    let value = builder.build_int_add(value, right4, "ctpop.bytes.unmasked")?;
    let value = builder.build_and(value, m4, "ctpop.bytes")?;
    let value = builder.build_int_mul(value, h01, "ctpop.sum")?;
    builder
        .build_right_shift(value, i64_type.const_int(56, false), false, "ctpop.result")
        .map_err(Into::into)
}

fn build_bswap_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let m8 = i64_type.const_int(0x00ff_00ff_00ff_00ff, false);
    let m16 = i64_type.const_int(0x0000_ffff_0000_ffff, false);
    let left8 = builder.build_left_shift(
        builder.build_and(value, m8, "bswap.low8")?,
        i64_type.const_int(8, false),
        "bswap.left8",
    )?;
    let right8 = builder.build_and(
        builder.build_right_shift(value, i64_type.const_int(8, false), false, "bswap.shr8")?,
        m8,
        "bswap.right8",
    )?;
    let value = builder.build_or(left8, right8, "bswap.swap8")?;
    let left16 = builder.build_left_shift(
        builder.build_and(value, m16, "bswap.low16")?,
        i64_type.const_int(16, false),
        "bswap.left16",
    )?;
    let right16 = builder.build_and(
        builder.build_right_shift(value, i64_type.const_int(16, false), false, "bswap.shr16")?,
        m16,
        "bswap.right16",
    )?;
    let value = builder.build_or(left16, right16, "bswap.swap16")?;
    let left32 = builder.build_left_shift(value, i64_type.const_int(32, false), "bswap.left32")?;
    let right32 = builder.build_right_shift(value, i64_type.const_int(32, false), false, "bswap.right32")?;
    let value = builder.build_or(left32, right32, "bswap.swap32")?;
    shift_reversed_to_width(builder, i64_type, value, width, "bswap.result")
}

fn build_bitreverse_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let value = swap_bit_groups(builder, i64_type, value, 1, 0x5555_5555_5555_5555, "bitrev.1")?;
    let value = swap_bit_groups(builder, i64_type, value, 2, 0x3333_3333_3333_3333, "bitrev.2")?;
    let value = swap_bit_groups(builder, i64_type, value, 4, 0x0f0f_0f0f_0f0f_0f0f, "bitrev.4")?;
    let value = swap_bit_groups(builder, i64_type, value, 8, 0x00ff_00ff_00ff_00ff, "bitrev.8")?;
    let value = swap_bit_groups(builder, i64_type, value, 16, 0x0000_ffff_0000_ffff, "bitrev.16")?;
    let left32 = builder.build_left_shift(value, i64_type.const_int(32, false), "bitrev.left32")?;
    let right32 = builder.build_right_shift(value, i64_type.const_int(32, false), false, "bitrev.right32")?;
    let value = builder.build_or(left32, right32, "bitrev.swap32")?;
    shift_reversed_to_width(builder, i64_type, value, width, "bitrev.result")
}

fn swap_bit_groups<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    amount: u64,
    mask: u64,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let mask = i64_type.const_int(mask, false);
    let amount = i64_type.const_int(amount, false);
    let low = builder.build_left_shift(
        builder.build_and(value, mask, &format!("{name}.low"))?,
        amount,
        &format!("{name}.left"),
    )?;
    let high = builder.build_and(
        builder.build_right_shift(value, amount, false, &format!("{name}.shr"))?,
        mask,
        &format!("{name}.high"),
    )?;
    builder.build_or(low, high, &format!("{name}.or")).map_err(Into::into)
}

fn shift_reversed_to_width<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    width: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let shift = builder.build_int_sub(i64_type.const_int(64, false), width, &format!("{name}.shift"))?;
    builder.build_right_shift(value, shift, false, name).map_err(Into::into)
}

fn emit_float_bin_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: FloatBinOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let width = operands.get("width")?;
    let lhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "fbin.lhs.raw")?;
    let rhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "fbin.rhs.raw")?;
    let f32_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fbin.f32");
    let f64_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fbin.f64");
    let is_f32 = builder.build_int_compare(
        IntPredicate::EQ,
        width,
        ctx.i64_type.const_int(32, false),
        "fbin.is.f32",
    )?;
    builder.build_conditional_branch(is_f32, f32_block, f64_block)?;

    builder.position_at_end(f32_block);
    let value = build_f32_bin_bits(builder, ctx, lhs_raw, rhs_raw, op)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;

    builder.position_at_end(f64_block);
    let value = build_f64_bin_bits(builder, ctx, lhs_raw, rhs_raw, op)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_f32_bin_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    lhs_raw: IntValue<'ctx>,
    rhs_raw: IntValue<'ctx>,
    op: FloatBinOp,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let lhs_bits = builder.build_int_truncate(lhs_raw, i32_type, "fbin.lhs.i32")?;
    let rhs_bits = builder.build_int_truncate(rhs_raw, i32_type, "fbin.rhs.i32")?;
    let lhs = builder
        .build_bit_cast(lhs_bits, f32_type, "fbin.lhs.f32")?
        .into_float_value();
    let rhs = builder
        .build_bit_cast(rhs_bits, f32_type, "fbin.rhs.f32")?
        .into_float_value();
    let result = match op {
        FloatBinOp::Add => builder.build_float_add(lhs, rhs, "fbin.f32.add")?,
        FloatBinOp::Sub => builder.build_float_sub(lhs, rhs, "fbin.f32.sub")?,
        FloatBinOp::Mul => builder.build_float_mul(lhs, rhs, "fbin.f32.mul")?,
        FloatBinOp::Div => builder.build_float_div(lhs, rhs, "fbin.f32.div")?,
        FloatBinOp::Rem => build_float_remainder(builder, ctx.i64_type, lhs, rhs, "fbin.f32")?,
    };
    let bits = builder
        .build_bit_cast(result, i32_type, "fbin.f32.bits")?
        .into_int_value();
    Ok(builder.build_int_z_extend(bits, ctx.i64_type, "fbin.f32.bits64")?)
}

fn build_f64_bin_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    lhs_raw: IntValue<'ctx>,
    rhs_raw: IntValue<'ctx>,
    op: FloatBinOp,
) -> anyhow::Result<IntValue<'ctx>> {
    let f64_type = ctx.function.get_type().get_context().f64_type();
    let lhs = builder
        .build_bit_cast(lhs_raw, f64_type, "fbin.lhs.f64")?
        .into_float_value();
    let rhs = builder
        .build_bit_cast(rhs_raw, f64_type, "fbin.rhs.f64")?
        .into_float_value();
    let result = match op {
        FloatBinOp::Add => builder.build_float_add(lhs, rhs, "fbin.f64.add")?,
        FloatBinOp::Sub => builder.build_float_sub(lhs, rhs, "fbin.f64.sub")?,
        FloatBinOp::Mul => builder.build_float_mul(lhs, rhs, "fbin.f64.mul")?,
        FloatBinOp::Div => builder.build_float_div(lhs, rhs, "fbin.f64.div")?,
        FloatBinOp::Rem => build_float_remainder(builder, ctx.i64_type, lhs, rhs, "fbin.f64")?,
    };
    Ok(builder
        .build_bit_cast(result, ctx.i64_type, "fbin.f64.bits")?
        .into_int_value())
}

fn emit_float_unary_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: FloatUnaryOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let src = operands.get("src")?;
    let width = operands.get("width")?;
    let src_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "funary.src")?;

    let is_f32 = builder.build_int_compare(
        IntPredicate::EQ,
        width,
        ctx.i64_type.const_int(32, false),
        "funary.is.f32",
    )?;
    let current_block = builder
        .get_insert_block()
        .context("float unary handler has no insertion block")?;
    let function = current_block
        .get_parent()
        .context("float unary block has no function")?;
    let f32_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(function, "funary.f32");
    let f64_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(function, "funary.f64");
    builder.build_conditional_branch(is_f32, f32_block, f64_block)?;

    builder.position_at_end(f32_block);
    let f32_value = build_f32_unary_bits(builder, ctx, src_raw, op)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, f32_value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;

    builder.position_at_end(f64_block);
    let f64_value = build_f64_unary_bits(builder, ctx, src_raw, op)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, f64_value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_f32_unary_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    src_raw: IntValue<'ctx>,
    op: FloatUnaryOp,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let src_bits = builder.build_int_truncate(src_raw, i32_type, "funary.src.i32")?;
    let src = builder
        .build_bit_cast(src_bits, f32_type, "funary.src.f32")?
        .into_float_value();
    let result = match op {
        FloatUnaryOp::Neg => builder.build_float_neg(src, "funary.f32.neg")?,
    };
    let bits = builder
        .build_bit_cast(result, i32_type, "funary.f32.bits")?
        .into_int_value();
    Ok(builder.build_int_z_extend(bits, ctx.i64_type, "funary.f32.bits64")?)
}

fn build_f64_unary_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    src_raw: IntValue<'ctx>,
    op: FloatUnaryOp,
) -> anyhow::Result<IntValue<'ctx>> {
    let f64_type = ctx.function.get_type().get_context().f64_type();
    let src = builder
        .build_bit_cast(src_raw, f64_type, "funary.src.f64")?
        .into_float_value();
    let result = match op {
        FloatUnaryOp::Neg => builder.build_float_neg(src, "funary.f64.neg")?,
    };
    Ok(builder
        .build_bit_cast(result, ctx.i64_type, "funary.f64.bits")?
        .into_int_value())
}

#[derive(Debug, Clone, Copy)]
enum IntCastSignedness {
    Signed,
    Unsigned,
}

fn emit_float_cast_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: FloatCastOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let src = operands.get("src")?;
    let raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "fcast.src")?;
    match op {
        FloatCastOp::SignedIntToFloat => {
            emit_int_to_float_cast_handler(builder, operands, ctx, dst, raw, IntCastSignedness::Signed)
        },
        FloatCastOp::UnsignedIntToFloat => {
            emit_int_to_float_cast_handler(builder, operands, ctx, dst, raw, IntCastSignedness::Unsigned)
        },
        FloatCastOp::FloatToSignedInt => {
            emit_float_to_int_cast_handler(builder, operands, ctx, dst, raw, IntCastSignedness::Signed)
        },
        FloatCastOp::FloatToUnsignedInt => {
            emit_float_to_int_cast_handler(builder, operands, ctx, dst, raw, IntCastSignedness::Unsigned)
        },
        FloatCastOp::FloatTrunc => {
            let value = build_fptrunc_bits(builder, ctx, raw)?;
            finish_value_handler(builder, ctx, dst, value)
        },
        FloatCastOp::FloatExt => {
            let value = build_fpext_bits(builder, ctx, raw)?;
            finish_value_handler(builder, ctx, dst, value)
        },
    }
}

fn emit_int_to_float_cast_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    dst: IntValue<'ctx>,
    raw: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<()> {
    let from_width = operands.get("from_width")?;
    let to_width = operands.get("to_width")?;
    let is_f32 = builder.build_int_compare(
        IntPredicate::EQ,
        to_width,
        ctx.i64_type.const_int(32, false),
        "fcast.itof.is.f32",
    )?;
    let f32_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcast.itof.f32");
    let f64_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcast.itof.f64");
    builder.build_conditional_branch(is_f32, f32_block, f64_block)?;

    builder.position_at_end(f32_block);
    let f32_value = build_int_to_f32_bits(builder, ctx, raw, from_width, signedness)?;
    finish_value_handler(builder, ctx, dst, f32_value)?;

    builder.position_at_end(f64_block);
    let f64_value = build_int_to_f64_bits(builder, ctx, raw, from_width, signedness)?;
    finish_value_handler(builder, ctx, dst, f64_value)?;
    Ok(())
}

fn emit_float_to_int_cast_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    dst: IntValue<'ctx>,
    raw: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<()> {
    let from_width = operands.get("from_width")?;
    let to_width = operands.get("to_width")?;
    let is_f32 = builder.build_int_compare(
        IntPredicate::EQ,
        from_width,
        ctx.i64_type.const_int(32, false),
        "fcast.ftoi.is.f32",
    )?;
    let f32_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcast.ftoi.f32");
    let f64_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcast.ftoi.f64");
    builder.build_conditional_branch(is_f32, f32_block, f64_block)?;

    builder.position_at_end(f32_block);
    let f32_value = build_f32_to_int_bits(builder, ctx, raw, to_width, signedness)?;
    finish_value_handler(builder, ctx, dst, f32_value)?;

    builder.position_at_end(f64_block);
    let f64_value = build_f64_to_int_bits(builder, ctx, raw, to_width, signedness)?;
    finish_value_handler(builder, ctx, dst, f64_value)?;
    Ok(())
}

fn normalize_int_for_float_cast<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
    width: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<IntValue<'ctx>> {
    match signedness {
        IntCastSignedness::Signed => sign_extend_to_i64(builder, ctx.i64_type, raw, width),
        IntCastSignedness::Unsigned => mask_to_width(builder, ctx.i64_type, raw, width),
    }
}

fn build_int_to_f32_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
    from_width: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let int_value = normalize_int_for_float_cast(builder, ctx, raw, from_width, signedness)?;
    let float_value = match signedness {
        IntCastSignedness::Signed => builder.build_signed_int_to_float(int_value, f32_type, "fcast.sitofp.f32")?,
        IntCastSignedness::Unsigned => builder.build_unsigned_int_to_float(int_value, f32_type, "fcast.uitofp.f32")?,
    };
    let bits = builder
        .build_bit_cast(float_value, i32_type, "fcast.itof.f32.bits")?
        .into_int_value();
    Ok(builder.build_int_z_extend(bits, ctx.i64_type, "fcast.itof.f32.bits64")?)
}

fn build_int_to_f64_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
    from_width: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<IntValue<'ctx>> {
    let f64_type = ctx.function.get_type().get_context().f64_type();
    let int_value = normalize_int_for_float_cast(builder, ctx, raw, from_width, signedness)?;
    let float_value = match signedness {
        IntCastSignedness::Signed => builder.build_signed_int_to_float(int_value, f64_type, "fcast.sitofp.f64")?,
        IntCastSignedness::Unsigned => builder.build_unsigned_int_to_float(int_value, f64_type, "fcast.uitofp.f64")?,
    };
    Ok(builder
        .build_bit_cast(float_value, ctx.i64_type, "fcast.itof.f64.bits")?
        .into_int_value())
}

fn build_f32_to_int_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
    to_width: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let bits = builder.build_int_truncate(raw, i32_type, "fcast.ftoi.f32.src")?;
    let float_value = builder
        .build_bit_cast(bits, f32_type, "fcast.ftoi.f32")?
        .into_float_value();
    let int_value = match signedness {
        IntCastSignedness::Signed => {
            builder.build_float_to_signed_int(float_value, ctx.i64_type, "fcast.fptosi.f32")?
        },
        IntCastSignedness::Unsigned => {
            builder.build_float_to_unsigned_int(float_value, ctx.i64_type, "fcast.fptoui.f32")?
        },
    };
    mask_to_width(builder, ctx.i64_type, int_value, to_width)
}

fn build_f64_to_int_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
    to_width: IntValue<'ctx>,
    signedness: IntCastSignedness,
) -> anyhow::Result<IntValue<'ctx>> {
    let f64_type = ctx.function.get_type().get_context().f64_type();
    let float_value = builder
        .build_bit_cast(raw, f64_type, "fcast.ftoi.f64")?
        .into_float_value();
    let int_value = match signedness {
        IntCastSignedness::Signed => {
            builder.build_float_to_signed_int(float_value, ctx.i64_type, "fcast.fptosi.f64")?
        },
        IntCastSignedness::Unsigned => {
            builder.build_float_to_unsigned_int(float_value, ctx.i64_type, "fcast.fptoui.f64")?
        },
    };
    mask_to_width(builder, ctx.i64_type, int_value, to_width)
}

fn build_fptrunc_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let f64_type = llvm_ctx.f64_type();
    let float_value = builder
        .build_bit_cast(raw, f64_type, "fcast.fptrunc.src")?
        .into_float_value();
    let truncated = builder.build_float_trunc(float_value, f32_type, "fcast.fptrunc")?;
    let bits = builder
        .build_bit_cast(truncated, i32_type, "fcast.fptrunc.bits")?
        .into_int_value();
    Ok(builder.build_int_z_extend(bits, ctx.i64_type, "fcast.fptrunc.bits64")?)
}

fn build_fpext_bits<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    raw: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let f64_type = llvm_ctx.f64_type();
    let bits = builder.build_int_truncate(raw, i32_type, "fcast.fpext.src")?;
    let float_value = builder
        .build_bit_cast(bits, f32_type, "fcast.fpext.f32")?
        .into_float_value();
    let extended = builder.build_float_ext(float_value, f64_type, "fcast.fpext")?;
    Ok(builder
        .build_bit_cast(extended, ctx.i64_type, "fcast.fpext.bits")?
        .into_int_value())
}

fn finish_value_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    dst: IntValue<'ctx>,
    value: IntValue<'ctx>,
) -> anyhow::Result<()> {
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_float_remainder<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    int_type: IntType<'ctx>,
    lhs: FloatValue<'ctx>,
    rhs: FloatValue<'ctx>,
    name: &str,
) -> anyhow::Result<FloatValue<'ctx>> {
    let quotient = builder.build_float_div(lhs, rhs, &format!("{name}.rem.quot"))?;
    let truncated = builder.build_float_to_signed_int(quotient, int_type, &format!("{name}.rem.trunc"))?;
    let truncated_float =
        builder.build_signed_int_to_float(truncated, lhs.get_type(), &format!("{name}.rem.trunc.f"))?;
    let product = builder.build_float_mul(truncated_float, rhs, &format!("{name}.rem.product"))?;
    Ok(builder.build_float_sub(lhs, product, &format!("{name}.rem"))?)
}

fn emit_icmp_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let pred = operands.get("pred")?;
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let width = operands.get("width")?;
    let selected = build_icmp_result(builder, ctx, pred, lhs, rhs, width, "icmp")?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, selected)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn emit_super_icmp_br_if_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let pred = operands.get("pred")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let width = operands.get("width")?;
    let then_pc = operands.get("then_pc")?;
    let else_pc = operands.get("else_pc")?;
    let selected = build_icmp_result(builder, ctx, pred, lhs, rhs, width, "icmp_br_if")?;
    let is_true =
        builder.build_int_compare(IntPredicate::NE, selected, ctx.i64_type.const_zero(), "icmp_br_if.true")?;
    let next_pc = builder
        .build_select(is_true, then_pc, else_pc, "icmp_br_if.next.pc")?
        .into_int_value();
    builder.build_store(ctx.pc_ptr, next_pc)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_icmp_result<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    pred: IntValue<'ctx>,
    lhs: IntValue<'ctx>,
    rhs: IntValue<'ctx>,
    width: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let lhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "icmp.lhs")?;
    let rhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "icmp.rhs")?;
    let lhs_u = mask_to_width(builder, ctx.i64_type, lhs_raw, width)?;
    let rhs_u = mask_to_width(builder, ctx.i64_type, rhs_raw, width)?;
    let lhs_s = sign_extend_to_i64(builder, ctx.i64_type, lhs_raw, width)?;
    let rhs_s = sign_extend_to_i64(builder, ctx.i64_type, rhs_raw, width)?;

    let comparisons = [
        (0, builder.build_int_compare(IntPredicate::EQ, lhs_u, rhs_u, "cmp.eq")?),
        (1, builder.build_int_compare(IntPredicate::NE, lhs_u, rhs_u, "cmp.ne")?),
        (
            2,
            builder.build_int_compare(IntPredicate::UGT, lhs_u, rhs_u, "cmp.ugt")?,
        ),
        (
            3,
            builder.build_int_compare(IntPredicate::UGE, lhs_u, rhs_u, "cmp.uge")?,
        ),
        (
            4,
            builder.build_int_compare(IntPredicate::ULT, lhs_u, rhs_u, "cmp.ult")?,
        ),
        (
            5,
            builder.build_int_compare(IntPredicate::ULE, lhs_u, rhs_u, "cmp.ule")?,
        ),
        (
            6,
            builder.build_int_compare(IntPredicate::SGT, lhs_s, rhs_s, "cmp.sgt")?,
        ),
        (
            7,
            builder.build_int_compare(IntPredicate::SGE, lhs_s, rhs_s, "cmp.sge")?,
        ),
        (
            8,
            builder.build_int_compare(IntPredicate::SLT, lhs_s, rhs_s, "cmp.slt")?,
        ),
        (
            9,
            builder.build_int_compare(IntPredicate::SLE, lhs_s, rhs_s, "cmp.sle")?,
        ),
    ];
    let mut selected = ctx.i64_type.const_zero();
    for (tag, cmp) in comparisons {
        let tag_match =
            builder.build_int_compare(IntPredicate::EQ, pred, ctx.i64_type.const_int(tag, false), "pred.match")?;
        let cmp64 = builder.build_int_z_extend(cmp, ctx.i64_type, "cmp64")?;
        selected = builder
            .build_select(tag_match, cmp64, selected, &format!("{name}.selected"))?
            .into_int_value();
    }

    Ok(selected)
}

fn emit_fcmp_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    let pred = operands.get("pred")?;
    let dst = operands.get("dst")?;
    let lhs = operands.get("lhs")?;
    let rhs = operands.get("rhs")?;
    let width = operands.get("width")?;
    let lhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, lhs, "fcmp.lhs.raw")?;
    let rhs_raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, rhs, "fcmp.rhs.raw")?;
    let f32_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcmp.f32");
    let f64_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "fcmp.f64");
    let is_f32 = builder.build_int_compare(
        IntPredicate::EQ,
        width,
        ctx.i64_type.const_int(32, false),
        "fcmp.is.f32",
    )?;
    builder.build_conditional_branch(is_f32, f32_block, f64_block)?;

    builder.position_at_end(f32_block);
    let value = build_f32_cmp_value(builder, ctx, pred, lhs_raw, rhs_raw)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;

    builder.position_at_end(f64_block);
    let value = build_f64_cmp_value(builder, ctx, pred, lhs_raw, rhs_raw)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn build_f32_cmp_value<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    pred: IntValue<'ctx>,
    lhs_raw: IntValue<'ctx>,
    rhs_raw: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let llvm_ctx = ctx.function.get_type().get_context();
    let i32_type = llvm_ctx.i32_type();
    let f32_type = llvm_ctx.f32_type();
    let lhs_bits = builder.build_int_truncate(lhs_raw, i32_type, "fcmp.lhs.i32")?;
    let rhs_bits = builder.build_int_truncate(rhs_raw, i32_type, "fcmp.rhs.i32")?;
    let lhs = builder
        .build_bit_cast(lhs_bits, f32_type, "fcmp.lhs.f32")?
        .into_float_value();
    let rhs = builder
        .build_bit_cast(rhs_bits, f32_type, "fcmp.rhs.f32")?
        .into_float_value();
    select_float_compare(builder, ctx.i64_type, pred, lhs, rhs)
}

fn build_f64_cmp_value<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    pred: IntValue<'ctx>,
    lhs_raw: IntValue<'ctx>,
    rhs_raw: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let f64_type = ctx.function.get_type().get_context().f64_type();
    let lhs = builder
        .build_bit_cast(lhs_raw, f64_type, "fcmp.lhs.f64")?
        .into_float_value();
    let rhs = builder
        .build_bit_cast(rhs_raw, f64_type, "fcmp.rhs.f64")?
        .into_float_value();
    select_float_compare(builder, ctx.i64_type, pred, lhs, rhs)
}

fn select_float_compare<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    pred: IntValue<'ctx>,
    lhs: FloatValue<'ctx>,
    rhs: FloatValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let mut selected = i64_type.const_zero();
    for (tag, llvm_predicate) in float_predicate_tags() {
        let cmp = builder.build_float_compare(llvm_predicate, lhs, rhs, "fcmp.value")?;
        let tag_match = builder.build_int_compare(
            IntPredicate::EQ,
            pred,
            i64_type.const_int(tag, false),
            "fcmp.pred.match",
        )?;
        let cmp64 = builder.build_int_z_extend(cmp, i64_type, "fcmp.value64")?;
        selected = builder
            .build_select(tag_match, cmp64, selected, "fcmp.selected")?
            .into_int_value();
    }
    Ok(selected)
}

fn float_predicate_tags() -> [(u64, LlvmFloatPredicate); 16] {
    [
        (VmFloatPredicate::False as u64, LlvmFloatPredicate::PredicateFalse),
        (VmFloatPredicate::Oeq as u64, LlvmFloatPredicate::OEQ),
        (VmFloatPredicate::Ogt as u64, LlvmFloatPredicate::OGT),
        (VmFloatPredicate::Oge as u64, LlvmFloatPredicate::OGE),
        (VmFloatPredicate::Olt as u64, LlvmFloatPredicate::OLT),
        (VmFloatPredicate::Ole as u64, LlvmFloatPredicate::OLE),
        (VmFloatPredicate::One as u64, LlvmFloatPredicate::ONE),
        (VmFloatPredicate::Ord as u64, LlvmFloatPredicate::ORD),
        (VmFloatPredicate::Uno as u64, LlvmFloatPredicate::UNO),
        (VmFloatPredicate::Ueq as u64, LlvmFloatPredicate::UEQ),
        (VmFloatPredicate::Ugt as u64, LlvmFloatPredicate::UGT),
        (VmFloatPredicate::Uge as u64, LlvmFloatPredicate::UGE),
        (VmFloatPredicate::Ult as u64, LlvmFloatPredicate::ULT),
        (VmFloatPredicate::Ule as u64, LlvmFloatPredicate::ULE),
        (VmFloatPredicate::Une as u64, LlvmFloatPredicate::UNE),
        (VmFloatPredicate::True as u64, LlvmFloatPredicate::PredicateTrue),
    ]
}

fn emit_cast_handler<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    operands: HandlerOperands<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    op: CastOp,
) -> anyhow::Result<()> {
    let dst = operands.get("dst")?;
    let src = operands.get("src")?;
    let from_width = operands.get("from_width")?;
    let to_width = operands.get("to_width")?;
    let raw = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "cast.src")?;
    let value = match op {
        CastOp::ZExt | CastOp::Trunc | CastOp::Bitcast => mask_to_width(builder, ctx.i64_type, raw, to_width)?,
        CastOp::SExt => {
            let extended = sign_extend_to_i64(builder, ctx.i64_type, raw, from_width)?;
            mask_to_width(builder, ctx.i64_type, extended, to_width)?
        },
    };
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr, ctx.decoded_width)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
}

fn read_token<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    reader: FunctionValue<'ctx>,
    args: RuntimeArgs<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    call_reader(builder, reader, args.code, args.len, args.key, args.offset_ptr, name)
}

fn read_const_pool_value<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
    index: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let call = builder.build_call(
        ctx.read_const,
        &[
            ctx.const_pool.into(),
            ctx.const_pool_len.into(),
            ctx.key.into(),
            index.into(),
        ],
        "const.pool.read",
    )?;
    Ok(call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| anyhow::anyhow!("const_pool reader should return i64"))?
        .into_int_value())
}

fn call_reader<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    reader: FunctionValue<'ctx>,
    code: PointerValue<'ctx>,
    len: IntValue<'ctx>,
    key: IntValue<'ctx>,
    offset_ptr: PointerValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let call = builder.build_call(reader, &[code.into(), len.into(), key.into(), offset_ptr.into()], name)?;
    Ok(call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| anyhow::anyhow!("bytecode reader should return i64"))?
        .into_int_value())
}

fn load_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    ptr: PointerValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    Ok(builder.build_load2(i64_type, ptr, name)?.into_int_value())
}

fn load_reg<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    x_type: ArrayType<'ctx>,
    regs: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    let ptr = reg_ptr(builder, i64_type, x_type, regs, index, "reg.load.ptr")?;
    load_i64(builder, i64_type, ptr, name)
}

fn store_reg<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    x_type: ArrayType<'ctx>,
    regs: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    value: IntValue<'ctx>,
) -> anyhow::Result<()> {
    let ptr = reg_ptr(builder, i64_type, x_type, regs, index, "reg.store.ptr")?;
    builder.build_store(ptr, value)?;
    Ok(())
}

fn store_return_slots<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    ctx: HandlerContext<'ctx, '_>,
) -> anyhow::Result<()> {
    for (slot, register) in ctx.return_regs.iter().enumerate() {
        let value = load_reg(
            builder,
            ctx.i64_type,
            ctx.x_type,
            ctx.regs,
            ctx.i64_type.const_int(*register as u64, false),
            "ret.slot.value",
        )?;
        let slot_ptr = builder.build_gep2(
            ctx.i64_type,
            ctx.ret_slots,
            &[ctx.i64_type.const_int(slot as u64, false)],
            "ret.slot.ptr",
        )?;
        builder.build_store(slot_ptr, value)?;
    }
    Ok(())
}

fn reg_ptr<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    x_type: ArrayType<'ctx>,
    regs: PointerValue<'ctx>,
    index: IntValue<'ctx>,
    name: &str,
) -> anyhow::Result<PointerValue<'ctx>> {
    let zero = i64_type.const_zero();
    Ok(builder.build_in_bounds_gep2(x_type, regs, &[zero, index], name)?)
}

fn increment_pc<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    pc_ptr: PointerValue<'ctx>,
    decoded_width: u8,
) -> anyhow::Result<()> {
    let pc = load_i64(builder, i64_type, pc_ptr, "pc.old")?;
    let next = builder.build_int_add(pc, i64_type.const_int(decoded_width as u64, false), "pc.next")?;
    builder.build_store(pc_ptr, next)?;
    Ok(())
}

fn rotate_right_i8<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i8_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    amount: u8,
) -> anyhow::Result<IntValue<'ctx>> {
    let amount = amount % 8;
    if amount == 0 {
        return Ok(value);
    }
    let right = builder.build_right_shift(value, i8_type.const_int(amount as u64, false), false, "ror.right")?;
    let left_amount = 8 - amount;
    let left = builder.build_left_shift(value, i8_type.const_int(left_amount as u64, false), "ror.left")?;
    Ok(builder.build_or(left, right, "ror")?)
}

fn width_to_byte_count<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let rounded = builder.build_int_add(width, i64_type.const_int(7, false), "width.rounded")?;
    Ok(builder.build_right_shift(rounded, i64_type.const_int(3, false), false, "width.bytes")?)
}

fn decode_byte_from_profile<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    profile: &ProfilePackage,
    i8_type: IntType<'ctx>,
    i64_type: IntType<'ctx>,
    raw: IntValue<'ctx>,
    key: IntValue<'ctx>,
    offset: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let mut decoded = raw;
    for step in &profile.decoder.steps {
        match step {
            DecoderStep::XorStream => {
                let key_byte = key_stream_byte(builder, i8_type, i64_type, key, offset)?;
                decoded = builder.build_xor(decoded, key_byte, "decoded.xor")?;
            },
            DecoderStep::AddStream => {
                let key_byte = key_stream_byte(builder, i8_type, i64_type, key, offset)?;
                decoded = builder.build_int_sub(decoded, key_byte, "decoded.add_stream")?;
            },
            DecoderStep::Rol { amount } => {
                decoded = rotate_left_i8(builder, i8_type, decoded, *amount)?;
            },
            DecoderStep::Ror { amount } => {
                decoded = rotate_right_i8(builder, i8_type, decoded, *amount)?;
            },
            DecoderStep::VarintDecode => break,
            DecoderStep::BitUnpack => {},
        }
    }
    Ok(decoded)
}

fn rotate_left_i8<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i8_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    amount: u8,
) -> anyhow::Result<IntValue<'ctx>> {
    let amount = amount % 8;
    if amount == 0 {
        return Ok(value);
    }
    let left = builder.build_left_shift(value, i8_type.const_int(amount as u64, false), "rol.left")?;
    let right_amount = 8 - amount;
    let right = builder.build_right_shift(value, i8_type.const_int(right_amount as u64, false), false, "rol.right")?;
    Ok(builder.build_or(left, right, "rol")?)
}

fn key_stream_byte<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i8_type: IntType<'ctx>,
    i64_type: IntType<'ctx>,
    key: IntValue<'ctx>,
    index: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let rotate_seed = builder.build_int_mul(index, i64_type.const_int(13, false), "key.rot.seed")?;
    let rotate = builder.build_and(rotate_seed, i64_type.const_int(63, false), "key.rot")?;
    let left = builder.build_left_shift(key, rotate, "key.rot.left")?;
    let right_amount = builder.build_and(
        builder.build_int_sub(i64_type.const_zero(), rotate, "key.rot.neg")?,
        i64_type.const_int(63, false),
        "key.rot.right.amount",
    )?;
    let right = builder.build_right_shift(key, right_amount, false, "key.rot.right")?;
    let rotated_key = builder.build_or(left, right, "key.rotated")?;

    let stream_mul = builder.build_int_mul(index, i64_type.const_int(0x9e37_79b9_7f4a_7c15, false), "key.index.mul")?;
    let index_left = builder.build_left_shift(index, i64_type.const_int(17, false), "key.index.left")?;
    let index_right = builder.build_right_shift(index, i64_type.const_int(47, false), false, "key.index.right")?;
    let index_rot = builder.build_or(index_left, index_right, "key.index.rot")?;
    let index_high = builder.build_right_shift(index, i64_type.const_int(7, false), false, "key.index.high")?;
    let mixed = builder.build_xor(rotated_key, stream_mul, "key.mix.0")?;
    let mixed = builder.build_xor(mixed, index_rot, "key.mix.1")?;
    let mixed = builder.build_xor(mixed, index_high, "key.mix.2")?;
    let folded = builder.build_xor(
        mixed,
        builder.build_right_shift(mixed, i64_type.const_int(32, false), false, "key.fold.32")?,
        "key.fold.0",
    )?;
    let folded = builder.build_xor(
        folded,
        builder.build_right_shift(mixed, i64_type.const_int(16, false), false, "key.fold.16")?,
        "key.fold.1",
    )?;
    let folded = builder.build_xor(
        folded,
        builder.build_right_shift(mixed, i64_type.const_int(8, false), false, "key.fold.8")?,
        "key.fold.2",
    )?;

    Ok(builder.build_int_truncate(folded, i8_type, "key.byte")?)
}

fn mask_to_width<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let mask = width_mask(builder, i64_type, width)?;
    Ok(builder.build_and(value, mask, "width.masked")?)
}

fn width_mask<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let is_64 = builder.build_int_compare(IntPredicate::EQ, width, i64_type.const_int(64, false), "width.is64")?;
    let shift = builder.build_and(width, i64_type.const_int(63, false), "width.shift")?;
    let one = i64_type.const_int(1, false);
    let shifted = builder.build_left_shift(one, shift, "mask.shifted")?;
    let candidate = builder.build_int_sub(shifted, one, "mask.candidate")?;
    Ok(builder
        .build_select(is_64, i64_type.const_all_ones(), candidate, "width.mask")?
        .into_int_value())
}

fn sign_extend_to_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: IntType<'ctx>,
    value: IntValue<'ctx>,
    width: IntValue<'ctx>,
) -> anyhow::Result<IntValue<'ctx>> {
    let masked = mask_to_width(builder, i64_type, value, width)?;
    let one = i64_type.const_int(1, false);
    let sign_shift = builder.build_int_sub(width, one, "sign.shift.raw")?;
    let sign_shift = builder.build_and(sign_shift, i64_type.const_int(63, false), "sign.shift")?;
    let sign_bit = builder.build_left_shift(one, sign_shift, "sign.bit")?;
    let is_negative = builder.build_int_compare(
        IntPredicate::NE,
        builder.build_and(masked, sign_bit, "sign.test")?,
        i64_type.const_zero(),
        "is.negative",
    )?;
    let mask = width_mask(builder, i64_type, width)?;
    let extend_mask = builder.build_xor(mask, i64_type.const_all_ones(), "extend.mask")?;
    let extended = builder.build_or(masked, extend_mask, "sign.extended")?;
    Ok(builder
        .build_select(is_negative, extended, masked, "sign.result")?
        .into_int_value())
}

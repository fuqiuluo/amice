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
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::types::{ArrayType, FunctionType, IntType, PointerType};
use amice_plugin::inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue, UnnamedAddress};
use amice_plugin::inkwell::{AddressSpace, IntPredicate};
use amice_vm::isa::{
    BinOp, CastOp, InstructionDesc, PcExpr, SemanticBinOp, SemanticExpr, SemanticProgram, SemanticStmt,
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
    let scan_loop = ctx.append_basic_block(function, "scan.loop");
    let skip_decode = ctx.append_basic_block(function, "skip.decode");
    let execute_decode = ctx.append_basic_block(function, "execute.decode");
    let default_return = ctx.append_basic_block(function, "default.return");

    builder.position_at_end(entry);
    let regs = builder.build_alloca(x_type, "x")?;
    // 即使内置 profile 声明 q.lowering = disabled，runtime state 仍保留固定 q0..q64 组。
    // 契约是 verifier 拒绝不支持的宽值 lowering，而不是让 VM 悄悄改变形状并丢掉 v128 寄存器组。
    let _q_regs = builder.build_alloca(q_type, "q")?;
    let pc_ptr = builder.build_alloca(i64_type, "pc")?;
    let offset_ptr = builder.build_alloca(i64_type, "offset")?;
    let scan_ptr = builder.build_alloca(i64_type, "scan")?;
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
    let inst_count = function.get_nth_param(5).unwrap().into_int_value();
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
    let pc_in_range = builder.build_int_compare(IntPredicate::ULT, pc, inst_count, "pc.in.range")?;
    builder.build_store(offset_ptr, i64_type.const_zero())?;
    builder.build_store(scan_ptr, i64_type.const_zero())?;
    builder.build_conditional_branch(pc_in_range, scan_loop, default_return)?;

    builder.position_at_end(scan_loop);
    // bytecode PC 是指令索引，不是字节偏移。dispatcher 从头扫描，解码并跳过 record，
    // 直到 scan == pc 后，再用同一套 decoder pipeline 执行选中的指令。
    let scan = load_i64(&builder, i64_type, scan_ptr, "scan")?;
    let pc = load_i64(&builder, i64_type, pc_ptr, "pc.current")?;
    let is_current = builder.build_int_compare(IntPredicate::EQ, scan, pc, "is.current")?;
    builder.build_conditional_branch(is_current, execute_decode, skip_decode)?;

    builder.position_at_end(skip_decode);
    let skip_opcode = read_token(
        &builder,
        read_varint,
        RuntimeArgs {
            code,
            len,
            key,
            offset_ptr,
        },
        "skip.opcode",
    )?;
    let skip_case_count = profile.isa.instructions.iter().map(|desc| desc.opcodes().len()).sum();
    let mut skip_cases = Vec::with_capacity(skip_case_count);
    let handler_alias_order = handler_alias_order(profile, name);
    // 跳过非当前 PC 的 record 时仍必须按 profile operand 数量解码 operand，
    // 否则 offset 会停在错误位置，后续扫描无法找到正确 record。
    for (instruction_index, opcode) in &handler_alias_order {
        let desc = &profile.isa.instructions[*instruction_index];
        let block = ctx.append_basic_block(function, &format!("skip.{}.op{opcode:02x}", desc.name));
        skip_cases.push((i64_type.const_int(*opcode as u64, false), block));
        builder.position_at_end(block);
        for _ in 0..desc.operands {
            let _ = read_token(
                &builder,
                read_operand,
                RuntimeArgs {
                    code,
                    len,
                    key,
                    offset_ptr,
                },
                "skip.operand",
            )?;
        }
        let next_scan = builder.build_int_add(scan, i64_type.const_int(1, false), "scan.next")?;
        builder.build_store(scan_ptr, next_scan)?;
        builder.build_unconditional_branch(scan_loop)?;
    }
    builder.position_at_end(skip_decode);
    builder.build_switch(skip_opcode, default_return, &skip_cases)?;

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
    Mov,
    Bin(BinOp),
    Icmp,
    Cast(CastOp),
    Alloca,
    Load,
    Store,
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
        } else if let Some(op) = bin_template(statements) {
            Self::Bin(op)
        } else if ashr_template(statements) {
            Self::Bin(BinOp::AShr)
        } else if has_assign_reg(statements, "dst", &compare_expr()) {
            Self::Icmp
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
        } else if store_template(statements) {
            Self::Store
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
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::ConstLoad => {
            let dst = operands.get("dst")?;
            let index = operands.get("index")?;
            let width = operands.get("width")?;
            let value = read_const_pool_value(builder, ctx, index)?;
            let value = mask_to_width(builder, ctx.i64_type, value, width)?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::Mov => {
            let dst = operands.get("dst")?;
            let src = operands.get("src")?;
            let width = operands.get("width")?;
            let value = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, src, "mov.src")?;
            let value = mask_to_width(builder, ctx.i64_type, value, width)?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, value)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::Bin(op) => {
            emit_bin_handler(builder, operands, ctx, op)?;
        },
        RuntimeHandlerTemplate::Icmp => {
            emit_icmp_handler(builder, operands, ctx)?;
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
        RuntimeHandlerTemplate::Gep => {
            let dst = operands.get("dst")?;
            let base = operands.get("base")?;
            let offset = operands.get("offset")?;
            let base_addr = load_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, base, "gep.base.addr")?;
            let addr = builder.build_int_add(base_addr, offset, "gep.addr")?;
            store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, addr)?;
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
            builder.build_unconditional_branch(ctx.loop_check)?;
        },
        RuntimeHandlerTemplate::CallNative => {
            emit_call_native_handler(builder, operands, ctx)?;
        },
        RuntimeHandlerTemplate::Nop => {
            increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
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
            let return_pc = builder.build_int_add(pc, ctx.i64_type.const_int(1, false), "vm.call.return.pc")?;
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

fn bin_template(statements: &[SemanticStmt]) -> Option<BinOp> {
    [
        (SemanticBinOp::Add, BinOp::Add),
        (SemanticBinOp::Sub, BinOp::Sub),
        (SemanticBinOp::Mul, BinOp::Mul),
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

fn compare_expr() -> SemanticExpr {
    SemanticExpr::Compare {
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

fn handler_alias_order(profile: &ProfilePackage, salt: &str) -> Vec<(usize, u8)> {
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
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
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
    let byte_count = width_to_byte_count(builder, ctx.i64_type, width)?;

    let index_ptr = builder.build_alloca(ctx.i64_type, "load.index")?;
    let result_ptr = builder.build_alloca(ctx.i64_type, "load.result")?;
    builder.build_store(index_ptr, ctx.i64_type.const_zero())?;
    builder.build_store(result_ptr, ctx.i64_type.const_zero())?;

    let loop_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "load.loop");
    let body_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "load.body");
    let done_block = ctx
        .function
        .get_type()
        .get_context()
        .append_basic_block(ctx.function, "load.done");
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(loop_block);
    let index = load_i64(builder, ctx.i64_type, index_ptr, "load.index.cur")?;
    let in_range = builder.build_int_compare(IntPredicate::ULT, index, byte_count, "load.in.range")?;
    builder.build_conditional_branch(in_range, body_block, done_block)?;

    builder.position_at_end(body_block);
    let byte_addr = builder.build_int_add(base_addr, index, "load.byte.addr")?;
    let byte_ptr = builder.build_int_to_ptr(byte_addr, ctx.ptr_type, "load.byte.ptr")?;
    let byte = builder
        .build_load2(ctx.i8_type, byte_ptr, "load.byte")?
        .into_int_value();
    let byte64 = builder.build_int_z_extend(byte, ctx.i64_type, "load.byte64")?;
    let shift = builder.build_int_mul(index, ctx.i64_type.const_int(8, false), "load.shift")?;
    let shifted = builder.build_left_shift(byte64, shift, "load.shifted")?;
    let old_result = load_i64(builder, ctx.i64_type, result_ptr, "load.result.old")?;
    let new_result = builder.build_or(old_result, shifted, "load.result.new")?;
    builder.build_store(result_ptr, new_result)?;
    let next_index = builder.build_int_add(index, ctx.i64_type.const_int(1, false), "load.index.next")?;
    builder.build_store(index_ptr, next_index)?;
    builder.build_unconditional_branch(loop_block)?;

    builder.position_at_end(done_block);
    let result = load_i64(builder, ctx.i64_type, result_ptr, "load.result.final")?;
    let result = mask_to_width(builder, ctx.i64_type, result, width)?;
    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, result)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
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
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
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
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
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
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
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
            .build_select(tag_match, cmp64, selected, "icmp.selected")?
            .into_int_value();
    }

    store_reg(builder, ctx.i64_type, ctx.x_type, ctx.regs, dst, selected)?;
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
    builder.build_unconditional_branch(ctx.loop_check)?;
    Ok(())
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
    increment_pc(builder, ctx.i64_type, ctx.pc_ptr)?;
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
) -> anyhow::Result<()> {
    let pc = load_i64(builder, i64_type, pc_ptr, "pc.old")?;
    let next = builder.build_int_add(pc, i64_type.const_int(1, false), "pc.next")?;
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

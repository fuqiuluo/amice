//! `vm_virtualize` pass 的 LLVM 侧接入层。
//!
//! # 主流程
//! - 读取全局配置、环境变量和函数注解，决定哪些函数启用 VMP。
//! - 加载并校验 profile，然后把 LLVM 函数 lowering 成 `amice-vm` 的 VM IR。
//! - 将 VM IR 编码成 bytecode，并按 profile scope 生成 per-function 或 module 级 bytecode blob。
//! - 生成 runtime dispatcher/native-call thunk，并把原函数替换成调用 dispatcher 的 wrapper。
//!
//! # 关键约束
//! 不支持的函数不能被半改写。`prepare_virtualization` 之前的检查、translator 的 `bail!`、
//! profile verifier 的错误都会被 pass 捕获为 safe-skip，原函数保持不变。

mod runtime;
mod translator;

use crate::config::{Config, VmVirtualizeConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BuilderExt, FunctionExt, ModuleExt};
use amice_llvm::{const_array, ptr_type};
use amice_macro::amice;
use amice_plugin::PreservedAnalyses;
use amice_plugin::inkwell::AddressSpace;
use amice_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use amice_plugin::inkwell::llvm_sys::core::{
    LLVMGetFirstUse, LLVMGetNextUse, LLVMGetNumOperands, LLVMGetOperand, LLVMGetUser, LLVMGetValueName2,
    LLVMIsACallInst, LLVMSetOperand, LLVMSetValueName2,
};
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use amice_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValueEnum, CallSiteValue, FunctionValue, GlobalValue, UnnamedAddress,
};
use amice_vm::bytecode::BytecodeEncoder;
use amice_vm::verify::verify_profile;
use amice_vm::{BytecodeImage, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, ProfilePackage, RuntimeScope};
use std::path::Path;

#[amice(
    priority = 955,
    name = "VmVirtualize",
    flag = AmicePassFlag::OptimizerLast | AmicePassFlag::FunctionLevel,
    config = VmVirtualizeConfig,
)]
#[derive(Default)]
pub struct VmVirtualize {}

impl AmicePass for VmVirtualize {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.vm_virtualize.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        // 先快照函数列表，再开始改写模块。后续会新增 wrapper/runtime/thunk，如果边遍历边插入，
        // 新增的 AMICE 内部函数可能被同一轮 pass 再次处理。
        let functions = module
            .get_functions()
            .into_iter()
            .filter(|function| !function.is_undef_function() && !function.is_llvm_function())
            .collect::<Vec<_>>();
        let mut changed = false;
        let mut module_bytecode_prepared = Vec::new();

        for function in functions {
            if is_amice_vm_symbol(function) {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            // 准备阶段只做“可失败但不改模块语义”的工作：profile 校验、LLVM→VM IR lowering、
            // bytecode 编码。只有这些全部成功后才进入真正的 wrapper/runtime 改写。
            match prepare_virtualization(module, function, &cfg) {
                Ok(prepared) => match prepared.profile.bytecode.scope {
                    RuntimeScope::Func => {
                        apply_function_bytecode_virtualization(module, prepared)?;
                        changed = true;
                    },
                    RuntimeScope::Module => {
                        module_bytecode_prepared.push(prepared);
                        changed = true;
                    },
                },
                Err(err) => {
                    debug!("skip function {:?}: {err:#}", function.get_name());
                },
            }
        }

        if !module_bytecode_prepared.is_empty() {
            // module scope 需要先收集所有函数的 bytecode，再统一拼接成一个共享 blob。
            // 每个 wrapper 仍通过 base offset 只访问自己的那一段 package。
            apply_module_bytecode_virtualizations(module, module_bytecode_prepared)?;
        }

        if changed {
            Ok(PreservedAnalyses::None)
        } else {
            Ok(PreservedAnalyses::All)
        }
    }
}

struct PreparedVirtualization<'ctx> {
    // 仍指向原 LLVM 函数；真正改写时会把它改名为 private body，再补一个同名 wrapper。
    original: FunctionValue<'ctx>,
    // 这里保留完整 profile，是因为 runtime/bytecode/wrapper 三处都要读取 ABI 和 scope。
    profile: ProfilePackage,
    // translator 提取出的宿主 ABI 视图，wrapper 用它把参数/返回值和 i64 VM ABI 互转。
    signature: translator::FunctionSignature,
    // 被 VM bytecode 中 call_native 指令引用的真实 LLVM callee 列表。
    native_calls: Vec<translator::NativeCallTarget<'ctx>>,
    // 已编码的 bytecode package，包含 header、const_pool、code、reloc 四类数据。
    bytecode: BytecodeImage,
    // 由源函数名清洗出的符号后缀，避免 LLVM symbol 里出现不适合内部符号的字符。
    safe_name: String,
}

fn prepare_virtualization<'ctx>(
    module: &mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    cfg: &VmVirtualizeConfig,
) -> anyhow::Result<PreparedVirtualization<'ctx>> {
    // 如果函数地址被非直接 call 使用，wrapper 替换无法保证所有 use 都被安全 retarget。
    // annotation metadata 是例外：它只服务配置读取，不代表运行期地址泄露。
    if has_unsupported_function_uses(function) {
        anyhow::bail!("function has non-call address uses");
    }

    let mut profile = load_profile(cfg)?;
    if let Some(scope) = cfg.runtime_scope {
        profile.runtime.scope = scope;
    }
    verify_profile(&profile)?;

    let translator::VmTranslation {
        function: vm_function,
        signature,
        native_calls,
    } = translator::translate_function(module, function, &profile.abi, &profile.lowering, &profile.isa)?;
    ensure_abi_covers_signature(&profile, &signature)?;
    if cfg.dump_lowering {
        debug!("lowering for {:?}: {vm_function:#?}", function.get_name());
    }

    let bytecode = BytecodeEncoder::new(&profile).encode(&vm_function)?;
    if cfg.dump_bytecode {
        debug!(
            "bytecode for {:?}: key=0x{:016x}, package_bytes={}, code_bytes={}, dump:\n{}",
            function.get_name(),
            bytecode.key,
            bytecode.bytes.len(),
            bytecode.code_len,
            bytecode.debug_dump
        );
    }

    let safe_name = safe_symbol_suffix(&vm_function.name);
    Ok(PreparedVirtualization {
        original: function,
        profile,
        signature,
        native_calls,
        bytecode,
        safe_name,
    })
}

fn apply_function_bytecode_virtualization<'ctx>(
    module: &mut Module<'ctx>,
    prepared: PreparedVirtualization<'ctx>,
) -> anyhow::Result<()> {
    // func scope 下每个函数携带自己的 bytecode global 和 runtime 符号，适合最大化 per-function
    // 多态；代价是模块内会生成更多内部函数。
    let bytecode_global = emit_bytecode_global(module, &prepared.safe_name, &prepared.bytecode)?;
    let native_table_global = emit_native_call_table(module, &prepared.safe_name, &prepared.native_calls)?;
    let _meta_global = emit_marker_global(module, &prepared.safe_name)?;
    let runtime = runtime::emit_runtime(
        module,
        &prepared.profile,
        prepared.profile.runtime.scope,
        &prepared.safe_name,
    )?;

    rewrite_as_wrapper(
        module,
        prepared.original,
        runtime.dispatch,
        bytecode_global,
        native_table_global,
        prepared.native_calls.len(),
        &prepared.bytecode,
        0,
        &prepared.signature,
        prepared.profile.abi.integer_returns.len(),
    )?;

    Ok(())
}

fn apply_module_bytecode_virtualizations<'ctx>(
    module: &mut Module<'ctx>,
    prepared: Vec<PreparedVirtualization<'ctx>>,
) -> anyhow::Result<()> {
    if prepared.is_empty() {
        return Ok(());
    }

    let placements = module_bytecode_placements(&prepared);
    let bytecode_global = emit_module_bytecode_global(module, &prepared)?;
    let _meta_global = emit_marker_global(module, "module")?;

    for (prepared, base_offset) in prepared.into_iter().zip(placements) {
        // bytecode blob 是 module 共享的，但 native thunk 仍按函数生成。call_native 的 callee
        // 类型取决于源函数内部调用点，不能在不同被保护函数之间盲目复用。
        let native_table_global = emit_native_call_table(module, &prepared.safe_name, &prepared.native_calls)?;
        let runtime = runtime::emit_runtime(
            module,
            &prepared.profile,
            prepared.profile.runtime.scope,
            &prepared.safe_name,
        )?;

        rewrite_as_wrapper(
            module,
            prepared.original,
            runtime.dispatch,
            bytecode_global,
            native_table_global,
            prepared.native_calls.len(),
            &prepared.bytecode,
            base_offset,
            &prepared.signature,
            prepared.profile.abi.integer_returns.len(),
        )?;
    }

    Ok(())
}

fn ensure_abi_covers_signature(
    profile: &ProfilePackage,
    signature: &translator::FunctionSignature,
) -> anyhow::Result<()> {
    if signature.param_widths.len() > profile.abi.integer_args.len() {
        anyhow::bail!(
            "profile ABI maps {} integer/pointer arguments but function needs {}",
            profile.abi.integer_args.len(),
            signature.param_widths.len()
        );
    }
    if !signature.returns_void && profile.abi.integer_returns.is_empty() {
        anyhow::bail!("profile ABI does not define ret0 for a non-void function");
    }
    let needed_returns = signature.return_slot_count();
    if needed_returns > profile.abi.integer_returns.len() {
        anyhow::bail!(
            "profile ABI maps {} return values but function needs {}",
            profile.abi.integer_returns.len(),
            needed_returns
        );
    }
    Ok(())
}

fn load_profile(cfg: &VmVirtualizeConfig) -> anyhow::Result<ProfilePackage> {
    let profile = match &cfg.profile_path {
        Some(path) => ProfilePackage::load_from_path(Path::new(path))?,
        None => ProfilePackage::builtin_test()?,
    };
    Ok(profile)
}

fn emit_bytecode_global<'ctx>(
    module: &mut Module<'ctx>,
    safe_name: &str,
    bytecode: &BytecodeImage,
) -> anyhow::Result<GlobalValue<'ctx>> {
    // bytecode 作为 private constant global 进入 IR，并加入 compiler.used，避免后续优化把只有
    // runtime 间接引用的数据删掉。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let values = bytecode
        .bytes
        .iter()
        .map(|byte| i8_type.const_int(*byte as u64, false))
        .collect::<Vec<_>>();
    let array = const_array(i8_type, &values);
    let global = module.add_global(
        i8_type.array_type(values.len() as u32),
        None,
        &format!(".amice.vm.bytecode.{safe_name}"),
    );
    global.set_initializer(&array);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    module.append_to_compiler_used(global);
    Ok(global)
}

fn module_bytecode_placements(prepared: &[PreparedVirtualization<'_>]) -> Vec<usize> {
    // 每个 BytecodeImage 本身已经是自包含 package；module scope 只是做字节级拼接，
    // 因此 wrapper 需要记录各自 package 在共享 blob 中的起始偏移。
    let mut offset = 0;
    prepared
        .iter()
        .map(|item| {
            let current = offset;
            offset += item.bytecode.bytes.len();
            current
        })
        .collect()
}

fn emit_module_bytecode_global<'ctx>(
    module: &mut Module<'ctx>,
    prepared: &[PreparedVirtualization<'ctx>],
) -> anyhow::Result<GlobalValue<'ctx>> {
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    // `bytecode.scope = module` 改变的是存储所有权，不是 dispatch ABI。每个函数仍在共享 blob
    // 内保留自包含 package，wrapper 只把该 package 的 code/const_pool slice 传给 runtime。
    let bytes = prepared
        .iter()
        .flat_map(|item| item.bytecode.bytes.iter().copied())
        .collect::<Vec<_>>();
    let values = bytes
        .iter()
        .map(|byte| i8_type.const_int(*byte as u64, false))
        .collect::<Vec<_>>();
    let array = const_array(i8_type, &values);
    let global = module.add_global(
        i8_type.array_type(values.len() as u32),
        None,
        ".amice.vm.bytecode.module",
    );
    global.set_initializer(&array);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    module.append_to_compiler_used(global);
    Ok(global)
}

fn emit_marker_global<'ctx>(module: &mut Module<'ctx>, safe_name: &str) -> anyhow::Result<GlobalValue<'ctx>> {
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let marker = b"AMICE_VMP_RUNTIME_BYTECODE\0";
    let values = marker
        .iter()
        .map(|byte| i8_type.const_int(*byte as u64, false))
        .collect::<Vec<_>>();
    let array = const_array(i8_type, &values);
    let global = module.add_global(
        i8_type.array_type(values.len() as u32),
        None,
        &format!(".amice.vm.meta.{safe_name}"),
    );
    global.set_initializer(&array);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    module.append_to_compiler_used(global);
    Ok(global)
}

fn emit_native_call_table<'ctx>(
    module: &mut Module<'ctx>,
    safe_name: &str,
    native_calls: &[translator::NativeCallTarget<'ctx>],
) -> anyhow::Result<GlobalValue<'ctx>> {
    // runtime 只知道固定 i64 调用 ABI。这里为每个真实 LLVM callee 建一个 thunk，
    // 再把 thunk 指针放进表里，bytecode 中的 call_id 就是这个表的索引。
    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let ptr_type = ptr_type!(ctx, i8_type);
    let _ = i8_type;
    let values = if native_calls.is_empty() {
        vec![ptr_type.const_null()]
    } else {
        native_calls
            .iter()
            .enumerate()
            .map(|(index, target)| {
                emit_native_call_thunk(module, safe_name, index, target)
                    .map(|thunk| thunk.as_global_value().as_pointer_value())
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    };

    let array = const_array(ptr_type, &values);
    let global = module.add_global(
        ptr_type.array_type(values.len() as u32),
        None,
        &format!(".amice.vm.native_table.{safe_name}"),
    );
    global.set_initializer(&array);
    global.set_constant(true);
    global.set_linkage(Linkage::Private);
    module.append_to_compiler_used(global);
    Ok(global)
}

fn emit_native_call_thunk<'ctx>(
    module: &mut Module<'ctx>,
    safe_name: &str,
    index: usize,
    target: &translator::NativeCallTarget<'ctx>,
) -> anyhow::Result<FunctionValue<'ctx>> {
    let ctx = module.get_context();
    let i64_type = ctx.i64_type();
    let thunk_arg_types = (0..NATIVE_CALL_MAX_ARGS).map(|_| i64_type.into()).collect::<Vec<_>>();
    let thunk_ret_type = ctx.struct_type(&vec![i64_type.into(); NATIVE_CALL_MAX_RETURNS], false);
    let thunk_type = thunk_ret_type.fn_type(&thunk_arg_types, false);
    let thunk = module.add_function(
        &format!(".amice.vm.native_thunk.{safe_name}.{index}"),
        thunk_type,
        Some(Linkage::Private),
    );
    thunk.as_global_value().set_unnamed_address(UnnamedAddress::Global);
    let entry = ctx.append_basic_block(thunk, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(entry);

    let args = target
        .function
        .get_type()
        .get_param_types()
        .iter()
        .enumerate()
        .map(|(arg_index, ty)| {
            // native thunk 是通用 VM x-register 值和 callee LLVM 函数类型之间的 ABI 防火墙。
            // 固定 thunk 宽度能让 bytecode call_native 保持 profile 可序列化，同时在真实调用点保留
            // 指针/整数重建逻辑。
            if target.param_is_pointer[arg_index] != matches!(ty, BasicMetadataTypeEnum::PointerType(_)) {
                anyhow::bail!("native thunk parameter kind mismatch");
            }
            let raw = thunk
                .get_nth_param(arg_index as u32)
                .ok_or_else(|| anyhow::anyhow!("missing native thunk parameter {arg_index}"))?
                .into_int_value();
            let value: BasicMetadataValueEnum<'ctx> = match ty {
                BasicMetadataTypeEnum::IntType(int_ty) => int_from_i64(&builder, raw, *int_ty)?.into(),
                BasicMetadataTypeEnum::PointerType(ptr_ty) => builder
                    .build_int_to_ptr(raw, *ptr_ty, "amice.vm.native.arg.ptr")?
                    .into(),
                _ => anyhow::bail!("native thunk target has a non-integer/non-pointer parameter"),
            };
            Ok(value)
        })
        .collect::<anyhow::Result<Vec<BasicMetadataValueEnum<'ctx>>>>()?;

    let call = builder.build_call(target.function, &args, "amice.vm.native.target")?;
    copy_function_attributes_to_call_site(call, target.function);
    let mut ret_tuple = thunk_ret_type.get_undef();
    if !target.returns_void {
        let ret = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| anyhow::anyhow!("native thunk target should return a value"))?;
        let returns = if target.return_fields.len() == 1 {
            vec![native_return_to_i64(&builder, i64_type, ret, target.return_fields[0])?]
        } else {
            let aggregate = ret.into_struct_value();
            target
                .return_fields
                .iter()
                .enumerate()
                .map(|(field_index, field)| {
                    let value =
                        builder.build_extract_value(aggregate, field_index as u32, "amice.vm.native.ret.field")?;
                    native_return_to_i64(&builder, i64_type, value, *field)
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        for (index, value) in returns.into_iter().take(NATIVE_CALL_MAX_RETURNS).enumerate() {
            ret_tuple = builder
                .build_insert_value(ret_tuple, value, index as u32, "amice.vm.native.ret.slot")?
                .into_struct_value();
        }
    }
    builder.build_return(Some(&ret_tuple))?;
    module.append_to_compiler_used(thunk.as_global_value());
    Ok(thunk)
}

fn rewrite_as_wrapper<'ctx>(
    module: &mut Module<'ctx>,
    original: FunctionValue<'ctx>,
    dispatch: FunctionValue<'ctx>,
    bytecode_global: GlobalValue<'ctx>,
    native_table_global: GlobalValue<'ctx>,
    native_call_count: usize,
    bytecode: &BytecodeImage,
    bytecode_base_offset: usize,
    signature: &translator::FunctionSignature,
    abi_return_count: usize,
) -> anyhow::Result<()> {
    // 改写策略是“原函数改名 + 新建同名 wrapper”。这样外部符号、调用约定和属性仍挂在
    // 原名称上，而原函数体保留为 private，供 direct-call retarget 和 native thunk 使用。
    let ctx = module.get_context();
    let i64_type = ctx.i64_type();
    let fn_type = original.get_type();
    let original_linkage = original.get_linkage();
    let original_name = original.get_name().to_string_lossy().into_owned();
    let preserved_name = format!(".amice.vm.original.{}", safe_symbol_suffix(&original_name));
    set_function_name(original, &preserved_name);
    original.set_linkage(Linkage::Private);
    original.as_global_value().set_unnamed_address(UnnamedAddress::Global);

    // 原始函数体会先保存在 private symbol 下，直到所有直接 call operand 完成 retarget。
    // 这样替换 public symbol 为 VM wrapper 时，不会让 annotation metadata use 失效。
    let wrapper = module.add_function(&original_name, fn_type, Some(original_linkage));
    copy_function_attributes(wrapper, original);
    wrapper.clear_stale_analysis_attrs_after_cfg_rewrite();

    let entry = ctx.append_basic_block(wrapper, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(entry);

    let ret_slot_count = signature.return_slot_count().max(abi_return_count).max(1);
    let ret_slots_type = i64_type.array_type(ret_slot_count as u32);
    let ret_slots = builder.build_alloca(ret_slots_type, "amice.vm.ret.slots")?;

    // dispatcher ABI 固定为：code/const_pool/native table/return slots 加 8 个 i64 参数槽。
    // wrapper 负责把原函数的整数和指针参数扩展或转换成 i64，再在返回时还原成原 LLVM 类型。
    let mut args = Vec::<BasicMetadataValueEnum<'ctx>>::with_capacity(17);
    let code_offset = i64_type.const_int((bytecode_base_offset + bytecode.code_offset) as u64, false);
    let code_ptr = builder.build_gep2(
        ctx.i8_type(),
        bytecode_global.as_pointer_value(),
        &[code_offset],
        "amice.vm.code.ptr",
    )?;
    let const_pool_offset = i64_type.const_int((bytecode_base_offset + bytecode.const_pool_offset) as u64, false);
    let const_pool_ptr = builder.build_gep2(
        ctx.i8_type(),
        bytecode_global.as_pointer_value(),
        &[const_pool_offset],
        "amice.vm.const_pool.ptr",
    )?;
    args.push(code_ptr.into());
    args.push(i64_type.const_int(bytecode.code_len as u64, false).into());
    args.push(const_pool_ptr.into());
    args.push(i64_type.const_int(bytecode.const_pool_len as u64, false).into());
    args.push(i64_type.const_int(bytecode.key, false).into());
    args.push(i64_type.const_int(bytecode.instruction_count as u64, false).into());
    args.push(native_table_global.as_pointer_value().into());
    args.push(i64_type.const_int(native_call_count as u64, false).into());
    args.push(ret_slots.into());

    for index in 0..8 {
        let value = if index < signature.param_widths.len() {
            let param = wrapper
                .get_nth_param(index as u32)
                .ok_or_else(|| anyhow::anyhow!("missing wrapper parameter {index}"))?;
            wrapper_param_to_i64(&builder, i64_type, param, signature.param_is_pointer[index])?
        } else {
            i64_type.const_zero()
        };
        args.push(value.into());
    }

    let call = builder.build_call(dispatch, &args, "amice.vm.ret")?;
    if signature.returns_void {
        // void 函数仍需要执行 dispatcher，因为副作用已经被 VM bytecode 表达；只是 wrapper
        // 不从返回寄存器取值。
        builder.build_return(None)?;
        redirect_direct_calls_to_wrapper(module, original, wrapper);
        return Ok(());
    }

    let ret64 = call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| anyhow::anyhow!("dispatch should return i64"))?
        .into_int_value();

    let return_type = fn_type
        .get_return_type()
        .ok_or_else(|| anyhow::anyhow!("wrapper return type unexpectedly void"))?;
    if signature.has_aggregate_return() {
        // 多字段 aggregate return 通过 ret_slots 返回。dispatcher 的 i64 返回值只保留标量快捷路径，
        // struct 字段需要逐槽重建回原 LLVM struct。
        let ret = rebuild_aggregate_return(&builder, i64_type, ret_slots_type, ret_slots, return_type, signature)?;
        builder.build_return(Some(&ret))?;
        redirect_direct_calls_to_wrapper(module, original, wrapper);
        return Ok(());
    }

    if signature.return_is_pointer {
        let ret = builder.build_int_to_ptr(ret64, return_type.into_pointer_type(), "amice.vm.ret.ptr")?;
        builder.build_return(Some(&ret))?;
    } else {
        let return_type = return_type.into_int_type();
        let ret = if signature.return_width == 64 {
            ret64
        } else {
            builder.build_int_truncate(ret64, return_type, "amice.vm.ret.trunc")?
        };
        builder.build_return(Some(&ret))?;
    }

    redirect_direct_calls_to_wrapper(module, original, wrapper);

    Ok(())
}

fn copy_function_attributes<'ctx>(target: FunctionValue<'ctx>, source: FunctionValue<'ctx>) {
    target.set_call_conventions(source.get_call_conventions());
    copy_function_attributes_at(target, source, AttributeLoc::Function);
    copy_function_attributes_at(target, source, AttributeLoc::Return);

    for index in 0..source.count_params() {
        copy_function_attributes_at(target, source, AttributeLoc::Param(index));
    }
}

fn copy_function_attributes_at<'ctx>(target: FunctionValue<'ctx>, source: FunctionValue<'ctx>, loc: AttributeLoc) {
    for attr in source.attributes(loc) {
        add_function_attribute_if_missing(target, loc, attr);
    }
}

fn add_function_attribute_if_missing(function: FunctionValue<'_>, loc: AttributeLoc, attr: Attribute) {
    if !function.attributes(loc).contains(&attr) {
        function.add_attribute(loc, attr);
    }
}

fn copy_function_attributes_to_call_site(call: CallSiteValue<'_>, source: FunctionValue<'_>) {
    call.set_call_convention(source.get_call_conventions());
    copy_function_attributes_to_call_site_at(call, source, AttributeLoc::Return);

    for index in 0..source.count_params() {
        copy_function_attributes_to_call_site_at(call, source, AttributeLoc::Param(index));
    }
}

fn copy_function_attributes_to_call_site_at(call: CallSiteValue<'_>, source: FunctionValue<'_>, loc: AttributeLoc) {
    for attr in source.attributes(loc) {
        add_call_site_attribute_if_missing(call, loc, attr);
    }
}

fn add_call_site_attribute_if_missing(call: CallSiteValue<'_>, loc: AttributeLoc, attr: Attribute) {
    if !call.attributes(loc).contains(&attr) {
        call.add_attribute(loc, attr);
    }
}

fn rebuild_aggregate_return<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: amice_plugin::inkwell::types::IntType<'ctx>,
    ret_slots_type: amice_plugin::inkwell::types::ArrayType<'ctx>,
    ret_slots: amice_plugin::inkwell::values::PointerValue<'ctx>,
    return_type: BasicTypeEnum<'ctx>,
    signature: &translator::FunctionSignature,
) -> anyhow::Result<BasicValueEnum<'ctx>> {
    let BasicTypeEnum::StructType(struct_type) = return_type else {
        anyhow::bail!("aggregate signature return type is not a struct");
    };
    let field_types = struct_type.get_field_types();
    if field_types.len() != signature.aggregate_return_fields.len() {
        anyhow::bail!(
            "aggregate return field count mismatch: signature has {}, LLVM type has {}",
            signature.aggregate_return_fields.len(),
            field_types.len()
        );
    }

    let zero = i64_type.const_zero();
    let mut aggregate = struct_type.get_undef();
    for (index, field_type) in field_types.into_iter().enumerate() {
        let slot_ptr = builder.build_in_bounds_gep2(
            ret_slots_type,
            ret_slots,
            &[zero, i64_type.const_int(index as u64, false)],
            "amice.vm.ret.slot.ptr",
        )?;
        let raw = builder
            .build_load2(i64_type, slot_ptr, "amice.vm.ret.slot.raw")?
            .into_int_value();
        let field = return_slot_to_value(builder, raw, field_type)?;
        aggregate = builder
            .build_insert_value(aggregate, field, index as u32, "amice.vm.ret.field")?
            .into_struct_value();
    }

    Ok(aggregate.into())
}

fn redirect_direct_calls_to_wrapper<'ctx>(
    module: &Module<'ctx>,
    original: FunctionValue<'ctx>,
    wrapper: FunctionValue<'ctx>,
) {
    for function in module.get_functions() {
        for block in function.get_basic_blocks() {
            for instruction in block.get_instructions() {
                if !is_direct_call_to(instruction.as_value_ref(), original.as_value_ref()) {
                    continue;
                }

                let callee_operand_index = instruction.get_num_operands().saturating_sub(1);
                // SAFETY: LLVM 把 direct callee 存在最后一个 call operand 中。`wrapper` 拥有和
                // 原函数完全相同的类型，因此只替换这个 operand 可以保持 call-site 类型正确，同时避免
                // module-wide RAUW 连 metadata annotation 一起改写。
                unsafe {
                    LLVMSetOperand(instruction.as_value_ref(), callee_operand_index, wrapper.as_value_ref());
                }
            }
        }
    }
}

fn has_unsupported_function_uses(function: FunctionValue<'_>) -> bool {
    // SAFETY: pass 正在 LLVM 内运行，`function` 和所有 user 都是 live 的。
    // 这里仅检查 use list 和 value name，不改变所有权。
    unsafe {
        let original = function.as_value_ref();
        let mut use_ref = LLVMGetFirstUse(original);
        while !use_ref.is_null() {
            let user = LLVMGetUser(use_ref);
            if !is_direct_call_to(user, original) && !reaches_global_annotations(user, 4) {
                return true;
            }
            use_ref = LLVMGetNextUse(use_ref);
        }
    }
    false
}

fn reaches_global_annotations(value: amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef, depth: u8) -> bool {
    // SAFETY: 这里递归遍历从函数 use 可达的 constant/global user。
    // 深度上限用于避免畸形 IR 中的病态环。
    unsafe {
        if value_name(value) == "llvm.global.annotations" {
            return true;
        }
        if depth == 0 {
            return false;
        }

        let mut use_ref = LLVMGetFirstUse(value);
        while !use_ref.is_null() {
            if reaches_global_annotations(LLVMGetUser(use_ref), depth - 1) {
                return true;
            }
            use_ref = LLVMGetNextUse(use_ref);
        }
    }
    false
}

fn is_direct_call_to(
    user: amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef,
    callee: amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef,
) -> bool {
    // SAFETY: `user` 是来自 use-list 或 instruction walk 的 LLVM value。
    // LLVM C API 会对非 call value 返回 null，因此探测是安全的。
    unsafe {
        if LLVMIsACallInst(user).is_null() {
            return false;
        }
        let operand_count = LLVMGetNumOperands(user);
        operand_count > 0 && LLVMGetOperand(user, (operand_count - 1) as u32) == callee
    }
}

fn value_name(value: amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef) -> String {
    // SAFETY: LLVM 在 `value` 生命周期内拥有返回指针；这里会立刻把字节复制到 owned Rust string。
    unsafe {
        let mut len = 0;
        let ptr = LLVMGetValueName2(value, &mut len);
        if ptr.is_null() {
            return String::new();
        }
        String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast::<u8>(), len)).into_owned()
    }
}

fn int_to_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: amice_plugin::inkwell::types::IntType<'ctx>,
    value: amice_plugin::inkwell::values::IntValue<'ctx>,
) -> anyhow::Result<amice_plugin::inkwell::values::IntValue<'ctx>> {
    let width = value.get_type().get_bit_width();
    if width == 64 {
        Ok(value)
    } else {
        Ok(builder.build_int_z_extend(value, i64_type, "amice.vm.arg.zext")?)
    }
}

fn wrapper_param_to_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: amice_plugin::inkwell::types::IntType<'ctx>,
    value: BasicValueEnum<'ctx>,
    is_pointer: bool,
) -> anyhow::Result<amice_plugin::inkwell::values::IntValue<'ctx>> {
    if is_pointer {
        Ok(builder.build_ptr_to_int(value.into_pointer_value(), i64_type, "amice.vm.arg.ptr")?)
    } else {
        int_to_i64(builder, i64_type, value.into_int_value())
    }
}

fn int_from_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    value: amice_plugin::inkwell::values::IntValue<'ctx>,
    target_type: amice_plugin::inkwell::types::IntType<'ctx>,
) -> anyhow::Result<amice_plugin::inkwell::values::IntValue<'ctx>> {
    if target_type.get_bit_width() == 64 {
        Ok(value)
    } else {
        Ok(builder.build_int_truncate(value, target_type, "amice.vm.native.arg.trunc")?)
    }
}

fn native_return_to_i64<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    i64_type: amice_plugin::inkwell::types::IntType<'ctx>,
    value: BasicValueEnum<'ctx>,
    field: translator::ReturnField,
) -> anyhow::Result<amice_plugin::inkwell::values::IntValue<'ctx>> {
    if field.is_pointer {
        return Ok(builder.build_ptr_to_int(value.into_pointer_value(), i64_type, "amice.vm.native.ret.ptr")?);
    }

    int_to_i64(builder, i64_type, value.into_int_value())
}

fn return_slot_to_value<'ctx>(
    builder: &amice_plugin::inkwell::builder::Builder<'ctx>,
    value: amice_plugin::inkwell::values::IntValue<'ctx>,
    target_type: BasicTypeEnum<'ctx>,
) -> anyhow::Result<BasicValueEnum<'ctx>> {
    match target_type {
        BasicTypeEnum::IntType(int_ty) => {
            if int_ty.get_bit_width() == 64 {
                Ok(value.into())
            } else {
                Ok(builder
                    .build_int_truncate(value, int_ty, "amice.vm.ret.field.trunc")?
                    .into())
            }
        },
        BasicTypeEnum::PointerType(ptr_ty) => Ok(builder
            .build_int_to_ptr(value, ptr_ty, "amice.vm.ret.field.ptr")?
            .into()),
        other => anyhow::bail!("unsupported aggregate return field type: {other:?}"),
    }
}

fn is_amice_vm_symbol(function: FunctionValue<'_>) -> bool {
    function
        .get_name()
        .to_str()
        .map(|name| name.starts_with(".amice.vm."))
        .unwrap_or(false)
}

fn set_function_name(function: FunctionValue<'_>, name: &str) {
    // SAFETY: `function` 是 live LLVM function value，LLVMSetValueName2 会精确复制
    // `name.len()` 个字节，因此输入不需要 NUL 结尾。
    unsafe {
        LLVMSetValueName2(function.as_value_ref(), name.as_bytes().as_ptr().cast(), name.len());
    }
}

fn safe_symbol_suffix(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

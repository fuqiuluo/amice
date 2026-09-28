use super::region;
use crate::config::BogusControlFlowConfig;
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, InstructionExt, VerifyResult};
use amice_plugin::inkwell::{
    DLLStorageClass, GlobalVisibility, IntPredicate,
    attributes::{Attribute, AttributeLoc},
    builder::Builder,
    context::AsContextRef,
    llvm_sys::{
        LLVMTypeKind,
        comdat::LLVMSetComdat,
        core::{
            LLVMBuildFreeze, LLVMGetCalledValue, LLVMGetParam, LLVMGetReturnType, LLVMGetTypeKind, LLVMIsAInlineAsm,
            LLVMSetCurrentDebugLocation2, LLVMTypeOf,
        },
        debuginfo::{LLVMDIBuilderCreateDebugLocation, LLVMGetSubprogram},
    },
    module::{Linkage, Module},
    types::AsTypeRef,
    values::{
        AsValueRef, BasicMetadataValueEnum, BasicValue, FunctionValue, InstructionOpcode as Op, InstructionValue,
        IntValue,
    },
};
use anyhow::{Context, ensure};
use rand::{Rng, SeedableRng, rngs::StdRng};

fn has_attribute(function: FunctionValue<'_>, location: AttributeLoc, name: &str) -> bool {
    let kind = Attribute::get_named_enum_kind_id(name);
    kind != 0 && function.get_enum_attribute(location, kind).is_some()
}

fn can_forward_signature(function: FunctionValue<'_>) -> bool {
    // Inkwell's BasicValue/BasicType enums panic on target extension, AMX and
    // token types. Check raw kinds before asking it to decode any parameters.
    let supported = |kind| {
        matches!(
            kind,
            LLVMTypeKind::LLVMIntegerTypeKind
                | LLVMTypeKind::LLVMPointerTypeKind
                | LLVMTypeKind::LLVMHalfTypeKind
                | LLVMTypeKind::LLVMBFloatTypeKind
                | LLVMTypeKind::LLVMFloatTypeKind
                | LLVMTypeKind::LLVMDoubleTypeKind
                | LLVMTypeKind::LLVMX86_FP80TypeKind
                | LLVMTypeKind::LLVMFP128TypeKind
                | LLVMTypeKind::LLVMPPC_FP128TypeKind
                | LLVMTypeKind::LLVMStructTypeKind
                | LLVMTypeKind::LLVMArrayTypeKind
                | LLVMTypeKind::LLVMVectorTypeKind
                | LLVMTypeKind::LLVMScalableVectorTypeKind
        )
    };
    // SAFETY: The function and its type are live; parameter indices are bounded
    // by LLVM's own count. No IR or wrapper for an unsupported value is created.
    unsafe {
        let result = LLVMGetTypeKind(LLVMGetReturnType(function.get_type().as_type_ref()));
        (result == LLVMTypeKind::LLVMVoidTypeKind || supported(result))
            && (0..function.count_params()).all(|index| {
                supported(LLVMGetTypeKind(LLVMTypeOf(LLVMGetParam(
                    function.as_value_ref(),
                    index,
                ))))
            })
    }
}

fn eligible(function: FunctionValue<'_>) -> bool {
    if function.count_basic_blocks() == 0
        || function.get_type().is_var_arg()
        || function.has_personality_function()
        || super::has_entry_block_intrinsics(function)
        || !can_forward_signature(function)
        || ["amice.bcf.done", "amice.bcf.generated"]
            .iter()
            .any(|key| function.get_string_attribute(AttributeLoc::Function, key).is_some())
        || [
            "naked",
            "returns_twice",
            "noduplicate",
            "convergent",
            "presplitcoroutine",
        ]
        .iter()
        .any(|key| has_attribute(function, AttributeLoc::Function, key))
        || !function.verify(true)
        || !function.is_inline_viable()
    {
        return false;
    }
    // These ABI parameters cannot be forwarded as ordinary call operands.
    if (0..function.count_params()).any(|index| {
        ["inalloca", "preallocated", "swifterror", "swiftasync", "nest"]
            .iter()
            .any(|key| has_attribute(function, AttributeLoc::Param(index), key))
    }) {
        return false;
    }
    function.get_basic_blocks().iter().all(|block| {
        !block.has_address_taken()
            && !block.is_eh_pad()
            && block
                .get_instructions()
                .all(|instruction| match instruction.get_opcode() {
                    Op::Invoke | Op::CallBr | Op::IndirectBr => false,
                    Op::Call => {
                        let instruction = instruction.into_call_inst();
                        // Opaque assembly can define labels or assembler state
                        // that cannot be duplicated, even on an unexecuted path.
                        // SAFETY: The opcode check guarantees a live CallBase.
                        if unsafe { !LLVMIsAInlineAsm(LLVMGetCalledValue(instruction.as_value_ref())).is_null() } {
                            return false;
                        }
                        let call = instruction.into_call_site_value();
                        let restricted = ["noduplicate", "convergent", "returns_twice"];
                        !restricted.iter().any(|key| {
                            call.get_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id(key))
                                .is_some()
                        }) && instruction.resolve_called_function().is_none_or(|callee| {
                            !restricted
                                .iter()
                                .any(|key| has_attribute(callee, AttributeLoc::Function, key))
                                && !callee.get_name().to_bytes().starts_with(b"llvm.coro.")
                        })
                    },
                    _ => true,
                })
    })
}

/// Only perturb arguments when every input bit pattern is accepted by the
/// copied arithmetic. Memory accesses, divisions, loops and range promises
/// instead keep their original arguments, including all pointer values.
fn can_perturb(function: FunctionValue<'_>) -> bool {
    let allowed_attributes = ["noundef", "signext", "zeroext", "inreg"].map(Attribute::get_named_enum_kind_id);
    function.count_basic_blocks() == 1
        && function.get_type().get_return_type().is_some_and(|ty| ty.is_int_type())
        && std::iter::once(AttributeLoc::Return)
            .chain((0..function.count_params()).map(AttributeLoc::Param))
            .all(|location| {
                function
                    .attributes(location)
                    .iter()
                    .all(|attribute| attribute.is_enum() && allowed_attributes.contains(&attribute.get_enum_kind_id()))
            })
        && function.get_param_iter().all(|param| param.is_int_value())
        && function.get_basic_blocks().iter().all(|block| {
            block.get_instructions().all(|instruction| {
                matches!(
                    instruction.get_opcode(),
                    Op::Add
                        | Op::Sub
                        | Op::Mul
                        | Op::And
                        | Op::Or
                        | Op::Xor
                        | Op::Trunc
                        | Op::ZExt
                        | Op::SExt
                        | Op::Return
                ) && !instruction.has_poison_generating_flags()
                    && !instruction.has_metadata()
            })
        })
}

/// Own all temporary definitions until the verified replacement is committed.
/// Callers are appended last, so reverse deletion removes their uses first.
#[derive(Default)]
struct Staging<'ctx> {
    functions: Vec<FunctionValue<'ctx>>,
}

impl<'ctx> Staging<'ctx> {
    fn track(&mut self, function: FunctionValue<'ctx>) -> FunctionValue<'ctx> {
        self.functions.push(function);
        function
    }

    fn snapshot(&mut self, function: FunctionValue<'ctx>, name: &str) -> anyhow::Result<FunctionValue<'ctx>> {
        let snapshot = function.clone_definition().context("cloning a complete BCF function")?;
        snapshot.as_global_value().set_name(name);
        Ok(self.track(snapshot))
    }
}

impl Drop for Staging<'_> {
    fn drop(&mut self) {
        for function in self.functions.iter().rev() {
            // SAFETY: Temporaries never escape staging. Original recursive
            // references still target the original function, not its clones.
            unsafe { function.delete() };
        }
    }
}

fn make_internal(function: FunctionValue<'_>) {
    let global = function.as_global_value();
    global.set_linkage(Linkage::Internal);
    global.set_visibility(GlobalVisibility::Default);
    global.set_dll_storage_class(DLLStorageClass::Default);
    // SAFETY: This live temporary is no longer a member of the public COMDAT.
    unsafe { LLVMSetComdat(function.as_value_ref(), std::ptr::null_mut()) };
    for name in ["noinline", "optnone"] {
        function.remove_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id(name));
    }
}

fn freeze<'ctx>(builder: &Builder<'ctx>, value: IntValue<'ctx>) -> IntValue<'ctx> {
    // SAFETY: The builder has a live insertion point and the operand is an
    // integer. One frozen value feeds both sides of the equality test.
    unsafe {
        IntValue::new(LLVMBuildFreeze(
            builder.as_mut_ptr(),
            value.as_value_ref(),
            c"bcf.input".as_ptr(),
        ))
    }
}

struct Guard<'ctx> {
    function: FunctionValue<'ctx>,
    addend: IntValue<'ctx>,
    mask: IntValue<'ctx>,
}

impl<'ctx> Guard<'ctx> {
    fn build(module: &Module<'ctx>, staging: &mut Staging<'ctx>, rng: &mut StdRng) -> anyhow::Result<Self> {
        let context = module.get_context();
        let ty = context.i32_type();
        let function = staging.track(module.add_function(
            "__amice_bcf_guard",
            ty.fn_type(&[ty.into()], false),
            Some(Linkage::Internal),
        ));
        let guard = Self {
            function,
            addend: ty.const_int(u64::from(rng.random::<u32>() | 1), false),
            mask: ty.const_int(u64::from(rng.random::<u32>() | 1), false),
        };
        let builder = context.create_builder();
        builder.position_at_end(context.append_basic_block(function, "entry"));
        let input = function
            .get_first_param()
            .expect("the guard has one integer parameter")
            .into_int_value();
        let result = guard.direct(&builder, input)?;
        builder.build_return(Some(&result))?;
        let config = BogusControlFlowConfig {
            probability: 100,
            max_regions: 1,
            ..Default::default()
        };
        // 守卫用独立预算把表达式转为循环，再与直接计算的结果比较，构造恒假条件。
        // 这次变换只作用于守卫，不能代替真实函数上的整数区域处理。
        ensure!(
            region::lower_to_digit_loops(module, function, &config, rng.random())? == 1,
            "BCF guard was not rewritten"
        );
        Ok(guard)
    }

    fn direct(&self, builder: &Builder<'ctx>, input: IntValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        let sum = builder.build_int_add(input, self.addend, "bcf.sum")?;
        Ok(builder.build_xor(sum, self.mask, "bcf.expected")?)
    }
}

/// 在同一事务中处理真实函数的整数区域，并插入、强制内联完整原始副本的虚假分支。
///
/// 位段循环有两处用途：守卫表达式用于构造不透明条件，真实函数体则按用户配置
/// 执行原有 BCF 区域变换。虚假路径的 snapshot 保留 BCF 改写前的完整函数体，
/// 不受区域指令预算截取。
///
/// `Ok(true)` 表示已完成区域处理并提交虚假分支，调用方无需重复处理区域；
/// 区域处理可能因没有候选而不产生改动。`Ok(false)` 表示未提交，原函数体
/// 保持原状，调用方可在其上执行原有区域变换作为回退。
///
/// # Errors
/// 创建副本或构造临时 IR 失败，或守卫未完成必要的区域变换时返回错误。
///
/// # Panics
/// 内部不变量被破坏，或提交后的函数未通过 LLVM 验证时 panic。
pub(super) fn insert_bogus_branch<'ctx>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    config: &BogusControlFlowConfig,
    seed: u64,
) -> anyhow::Result<bool> {
    if !config.clone
        || config.probability == 0
        || config.max_regions == 0
        || config.max_region_instructions < 2
        || !eligible(function)
    {
        return Ok(false);
    }
    let mut rng = StdRng::seed_from_u64(seed ^ 0x56e8_a175_1d3c_9b47);
    if rng.random::<u64>() % 100 >= u64::from(config.probability.min(100)) {
        return Ok(false);
    }
    let mut staging = Staging::default();
    // Both copies precede any BCF edits. The source snapshot remains whole;
    // only the second copy serves as a transaction for the real function.
    let snapshot = staging.snapshot(function, "__amice_bcf_snapshot")?;
    make_internal(snapshot);
    let perturb = can_perturb(snapshot);
    let guard = Guard::build(module, &mut staging, &mut rng)?;
    let staged = staging.snapshot(function, "__amice_bcf_staged")?;
    // 原有算法作用于真实路径的暂存副本；snapshot 的原始函数体不参与此次变换。
    // 区域变换与虚假分支统一提交，未提交时一并丢弃暂存副本。
    region::lower_to_digit_loops(module, staged, config, seed)?;

    let context = module.get_context();
    let entry = staged.get_first_basic_block().expect("eligible functions have a body");
    let dispatch = context.prepend_basic_block(entry, "bcf.dispatch");
    let fake = context.append_basic_block(staged, "bcf.fake");
    let builder = context.create_builder();
    // LLVM requires a call-site location when inlining a debug-info-bearing
    // callee. Line zero denotes generated code within the staged function.
    // SAFETY: The scope belongs to this function in the same live context;
    // a null inlined-at operand denotes a top-level generated location.
    unsafe {
        let scope = LLVMGetSubprogram(staged.as_value_ref());
        if !scope.is_null() {
            let location = LLVMDIBuilderCreateDebugLocation(context.as_ctx_ref(), 0, 0, scope, std::ptr::null_mut());
            LLVMSetCurrentDebugLocation2(builder.as_mut_ptr(), location);
        }
    }
    builder.position_at_end(dispatch);
    let ty = context.i32_type();
    let input = match staged.get_param_iter().find(|param| param.is_int_value()) {
        Some(value) => {
            builder.build_int_cast_sign_flag(freeze(&builder, value.into_int_value()), ty, false, "bcf.seed")?
        },
        None => ty.const_int(u64::from(rng.random::<u32>()), false),
    };
    let expected = guard.direct(&builder, input)?;
    let guard_call = builder.build_call(guard.function, &[input.into()], "bcf.actual")?;
    let actual = guard_call
        .try_as_basic_value()
        .basic()
        .expect("the guard returns i32")
        .into_int_value();
    let condition = builder.build_int_compare(IntPredicate::NE, actual, expected, "bcf.condition")?;
    builder.build_conditional_branch(condition, fake, entry)?;

    builder.position_at_end(fake);
    let arguments = staged
        .get_param_iter()
        .map(|value| {
            let value = if perturb {
                let integer = value.into_int_value();
                let delta = integer.get_type().const_int(rng.random::<u64>() | 1, false);
                builder
                    .build_int_add(freeze(&builder, integer), delta, "bcf.argument")?
                    .as_basic_value_enum()
            } else {
                value
            };
            Ok(BasicMetadataValueEnum::from(value))
        })
        .collect::<Result<Vec<_>, amice_plugin::inkwell::builder::BuilderError>>()?;
    let call = builder.build_call(snapshot, &arguments, "")?;
    call.set_call_convention(snapshot.get_call_conventions());
    for location in std::iter::once(AttributeLoc::Return).chain((0..snapshot.count_params()).map(AttributeLoc::Param)) {
        for attribute in snapshot.attributes(location) {
            call.add_attribute(location, attribute);
        }
    }
    match call.try_as_basic_value().basic() {
        Some(result) => builder.build_return(Some(&result))?,
        None => builder.build_return(None)?,
    };
    if !staged.verify(true) {
        return Ok(false);
    }
    // Explicit InlineFunction calls bypass profitability thresholds. A failed
    // legality check leaves the original definition intact and rolls back all
    // staging; a standalone snapshot call is never committed.
    // SAFETY: The call is live and verified; its handle is not reused. LLVM
    // retains the call's original block while splicing the callee into it.
    if !unsafe { InstructionValue::new(call.as_value_ref()).into_call_inst().inline() } {
        return Ok(false);
    }
    // InlineFunction hoists the copy's static allocas (including byval copies)
    // into dispatch. This would charge the real path for a second stack frame,
    // even though the copy cannot execute. Dispatch had no allocas beforehand;
    // move only those newly hoisted allocations back under the bogus branch.
    // Their users and lifetime markers are confined to the inlined fake path.
    let allocations = dispatch
        .get_instructions()
        .filter(|instruction| instruction.get_opcode() == Op::Alloca)
        .collect::<Vec<_>>();
    builder.position_before(
        &fake
            .get_first_instruction()
            .expect("inlining retains a terminated call block"),
    );
    for allocation in allocations {
        allocation.remove_from_basic_block();
        builder.insert_instruction(&allocation, None);
    }
    // SAFETY: Inlining the snapshot does not contain or remove the guard call.
    // Its handle is not reused after this operation.
    if !unsafe {
        InstructionValue::new(guard_call.as_value_ref())
            .into_call_inst()
            .inline()
    } {
        return Ok(false);
    }
    staged.clear_stale_analysis_attrs_after_cfg_rewrite();
    staged.add_attribute(
        AttributeLoc::Function,
        context.create_string_attribute("amice.bcf.done", "1"),
    );
    staged.add_attribute(
        AttributeLoc::Function,
        context.create_string_attribute("amice.bcf.clone", "1"),
    );
    if !staged.verify(true) {
        return Ok(false);
    }
    // SAFETY: The staged definition has the same type, module and original
    // global properties. Eligibility excludes escaping block addresses, and
    // no handle to the old body is retained past this commit.
    unsafe { function.replace_body_from(staged) };
    if let VerifyResult::Broken(reason) = function.verify_function() {
        panic!("BCF produced invalid IR after committing a function clone: {reason}");
    }
    Ok(true)
}

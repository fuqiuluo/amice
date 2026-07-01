//! LLVM IR 到 AMICE VM IR 的 lowering 层。
//!
//! # 读代码入口
//! `translate_function` 先抽取函数签名，再构造 `FunctionLowerer`。`FunctionLowerer::lower`
//! 按 basic block 顺序绑定 VM label，然后逐条 LLVM instruction 进入 `lower_*` 方法。
//!
//! # profile 驱动点
//! 这里不直接写死 opcode。每个 `lower_*` 方法只选择一个 lowering contract，例如
//! `llvm.add.integer` 或 `llvm.memory.scalar`，再执行 `lowering.vm` 中对应 action。
//! action 里的 `emit` 会保留 profile ISA 指令名，使后续 encoder 能选择 profile 声明的
//! opcode alias 和 operand 顺序。
//!
//! # safe-skip 契约
//! 此文件中遇到暂不支持的 LLVM IR 会返回 `Err`。上层 pass 捕获错误并保留原函数，
//! 因而这里宁可保守跳过，也不能生成不完整 VM IR。

use amice_llvm::inkwell2::{CallInst, FunctionExt, GepInst, InstructionExt, PhiInst, SwitchInst};
use amice_plugin::inkwell::IntPredicate;
use amice_plugin::inkwell::basic_block::BasicBlock;
use amice_plugin::inkwell::llvm_sys::LLVMTypeKind;
use amice_plugin::inkwell::llvm_sys::core::{LLVMGetElementType, LLVMGetGEPSourceElementType, LLVMGetTypeKind};
use amice_plugin::inkwell::llvm_sys::prelude::LLVMTypeRef;
use amice_plugin::inkwell::llvm_sys::target::LLVMStoreSizeOfType;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::targets::TargetData;
use amice_plugin::inkwell::types::{AnyTypeEnum, BasicMetadataTypeEnum, BasicTypeEnum};
use amice_plugin::inkwell::values::{AsValueRef, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue};
use amice_vm::abi::{AbiProfile, VmRegister};
use amice_vm::isa::{BinOp, CastOp, CmpPredicate, HandlerSemantic, InstructionDesc, IsaProfile, OperandKind};
use amice_vm::profile::{LoweringAction, LoweringProfile, LoweringRule, lowering_match_pattern};
use amice_vm::{
    LabelId, NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS, NativeReturn, VmFunction, VmFunctionBuilder, VmInstruction,
};
use anyhow::{Context, bail};
use std::collections::{HashMap, HashSet};

type ValueKey = usize;
type BlockKey = usize;

#[derive(Debug, Clone, Copy)]
struct ValueBinding {
    // VM x 寄存器编号。
    reg: u8,
    // 该寄存器当前承载的 LLVM 标量位宽；runtime 统一用 i64 存储，handler 按 width 截断。
    width: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnField {
    // aggregate/native return 字段的标量位宽。
    pub width: u8,
    // 字段是否需要在 wrapper/thunk 边界做 ptr<->i64 转换。
    pub is_pointer: bool,
}

#[derive(Debug)]
pub struct FunctionSignature {
    // 标量返回快捷路径使用的位宽；void 时保留为 64，实际不读取。
    pub return_width: u8,
    // 每个宿主参数映射到 VM ABI 时使用的位宽。
    pub param_widths: Vec<u8>,
    pub returns_void: bool,
    pub return_is_pointer: bool,
    pub param_is_pointer: Vec<bool>,
    // 非空表示直接 struct aggregate return；字段会通过 wrapper ret_slots 重建。
    pub aggregate_return_fields: Vec<ReturnField>,
}

impl FunctionSignature {
    pub fn return_slot_count(&self) -> usize {
        if self.returns_void {
            0
        } else {
            self.aggregate_return_fields.len().max(1)
        }
    }

    pub fn has_aggregate_return(&self) -> bool {
        !self.aggregate_return_fields.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct NativeCallTarget<'ctx> {
    // VM 内部 call_native 最终要调用的真实 LLVM 函数。
    pub function: FunctionValue<'ctx>,
    // thunk 用它把固定 i64 参数截断/转换回 callee 的真实参数类型。
    pub param_widths: Vec<u8>,
    pub returns_void: bool,
    // thunk 统一返回固定 i64 tuple；这里描述哪些 tuple slot 有效以及如何截断。
    pub return_fields: Vec<ReturnField>,
    pub param_is_pointer: Vec<bool>,
}

#[derive(Debug)]
pub struct VmTranslation<'ctx> {
    // 已完成 lowering 的 VM IR，仍未经过 bytecode encoder。
    pub function: VmFunction,
    // wrapper 改写需要的宿主 ABI 视图。
    pub signature: FunctionSignature,
    // bytecode 中 call_native 指令引用的 native thunk 目标。
    pub native_calls: Vec<NativeCallTarget<'ctx>>,
}

pub fn supported_signature(function: FunctionValue<'_>) -> anyhow::Result<FunctionSignature> {
    let fn_type = function.get_type();
    // signature 和 instruction lowering 里的每个 `bail!` 都是 safe-skip 契约的一部分：
    // module pass 会捕获错误、以 debug 级别记录原因，并保持原函数体不变，而不是生成半虚拟化 IR。
    if fn_type.is_var_arg() {
        bail!("varargs functions are not supported");
    }

    let mut aggregate_return_fields = Vec::new();
    let (returns_void, return_width, return_is_pointer) = match fn_type.get_return_type() {
        None => (true, 64, false),
        Some(BasicTypeEnum::IntType(return_type)) => (false, checked_width(return_type.get_bit_width())?, false),
        Some(BasicTypeEnum::PointerType(_)) => (false, 64, true),
        Some(BasicTypeEnum::StructType(return_type)) => {
            aggregate_return_fields = return_type
                .get_field_types()
                .into_iter()
                .enumerate()
                .map(|(index, ty)| return_field_from_type(ty).with_context(|| format!("return field {index}")))
                .collect::<anyhow::Result<Vec<_>>>()?;
            if aggregate_return_fields.is_empty() {
                bail!("empty aggregate returns are not supported");
            }
            (false, aggregate_return_fields[0].width, false)
        },
        Some(_) => bail!("only void, scalar integer, pointer, and direct struct aggregate returns are supported"),
    };

    let param_types = fn_type.get_param_types();
    if param_types.len() > 8 {
        bail!("only up to 8 scalar integer parameters are supported");
    }

    let params = param_types
        .iter()
        .map(|ty| match ty {
            BasicMetadataTypeEnum::IntType(int_ty) => Ok((checked_width(int_ty.get_bit_width())?, false)),
            BasicMetadataTypeEnum::PointerType(_) => Ok((64, true)),
            _ => bail!("only scalar integer and pointer parameters are supported"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (param_widths, param_is_pointer) = params.into_iter().unzip();

    Ok(FunctionSignature {
        return_width,
        param_widths,
        returns_void,
        return_is_pointer,
        param_is_pointer,
        aggregate_return_fields,
    })
}

pub fn translate_function<'ctx>(
    module: &mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    abi: &AbiProfile,
    lowering: &LoweringProfile,
    isa: &IsaProfile,
) -> anyhow::Result<VmTranslation<'ctx>> {
    let signature = supported_signature(function)?;
    let name = function.get_name().to_str().unwrap_or("<invalid-name>").to_owned();

    let lowerer = FunctionLowerer::new(module, function, &name, &signature, abi, lowering, isa)?;
    let (function, native_calls) = lowerer.lower()?;
    Ok(VmTranslation {
        function,
        signature,
        native_calls,
    })
}

struct FunctionLowerer<'m, 'ctx, 'profile> {
    module: &'m mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    // lowering.vm 的规则表。translator 通过 contract 名称查找规则，不从 LLVM opcode 直接写死 VM 指令。
    lowering: &'profile LoweringProfile,
    // isa.vm 的指令表。emit 阶段用它验证 profile operand 名称、kind 和 semantic。
    isa: &'profile IsaProfile,
    target_data: TargetData,
    builder: VmFunctionBuilder,
    // LLVM SSA value 到 VM x 寄存器的当前绑定。
    values: HashMap<ValueKey, ValueBinding>,
    // insertvalue/extractvalue 和多返回 native call 使用的临时 aggregate 绑定。
    aggregates: HashMap<ValueKey, AggregateBinding>,
    // LLVM basic block 到 VM bytecode label 的映射。
    labels: HashMap<BlockKey, LabelId>,
    native_calls: Vec<NativeCallTarget<'ctx>>,
    return_registers: Vec<u8>,
    native_arg_registers: Vec<u8>,
    native_return_registers: Vec<u8>,
    native_touched_registers: HashSet<u8>,
    aggregate_return_fields: Vec<ReturnField>,
    // 已经真正 materialize 到 VM 寄存器的 SSA value。native_call 保存 clobber 时依赖这个集合，
    // 避免保存尚未定义的虚拟值。
    defined_values: HashSet<ValueKey>,
    // 简单 liveness 复用计划。跨 block 值会 pinned，block-local 临时值在最后一次使用后释放。
    reuse_plan: Option<ReusePlan>,
    // 单条 lowering 过程中申请的 scratch 寄存器，指令结束后统一释放。
    temporary_regs: Vec<u8>,
}

#[derive(Debug, Clone)]
struct AggregateBinding {
    fields: Vec<Option<ValueBinding>>,
}

#[derive(Debug, Clone, Copy)]
struct RegisterMove {
    dst: u8,
    src: ValueBinding,
}

#[derive(Debug)]
struct ReusePlan {
    pinned_values: HashSet<ValueKey>,
    block_last_uses: HashMap<BlockKey, HashMap<ValueKey, usize>>,
    current_block: Option<BlockKey>,
    instruction_index: usize,
}

#[derive(Debug, Clone, Copy)]
enum LoweringValue {
    Reg(ValueBinding),
    Imm(u64),
    Label(LabelId),
}

#[derive(Debug, Default)]
struct LoweringEnv<'ctx> {
    // lowering.vm action 可引用的 VM 侧值，例如 `%va`、`%vr`、`type_width(%r)`。
    values: HashMap<String, LoweringValue>,
    // bind action 需要把 profile 里的 LLVM placeholder 反查到真实 SSA value。
    llvm_value_keys: HashMap<String, ValueKey>,
    // materialize action 可以从这里把 LLVM operand 拉进 VM 寄存器。
    llvm_sources: HashMap<String, BasicValueEnum<'ctx>>,
}

impl<'ctx> LoweringEnv<'ctx> {
    fn new() -> Self {
        Self::default()
    }

    fn binding(mut self, name: impl Into<String>, binding: ValueBinding) -> Self {
        self.values.insert(name.into(), LoweringValue::Reg(binding));
        self
    }

    fn llvm_value(mut self, name: impl Into<String>, key: ValueKey) -> Self {
        self.llvm_value_keys.insert(name.into(), key);
        self
    }

    fn llvm_source(mut self, name: impl Into<String>, value: BasicValueEnum<'ctx>) -> Self {
        self.llvm_sources.insert(name.into(), value);
        self
    }

    fn reg(mut self, name: impl Into<String>, reg: u8, width: u8) -> Self {
        self.values
            .insert(name.into(), LoweringValue::Reg(ValueBinding { reg, width }));
        self
    }

    fn imm(mut self, name: impl Into<String>, value: u64) -> Self {
        self.values.insert(name.into(), LoweringValue::Imm(value));
        self
    }

    fn label(mut self, name: impl Into<String>, label: LabelId) -> Self {
        self.values.insert(name.into(), LoweringValue::Label(label));
        self
    }

    fn get(&self, expr: &str) -> anyhow::Result<LoweringValue> {
        let expr = expr.trim();
        if let Some(value) = self.values.get(expr).copied() {
            return Ok(value);
        }
        if let Some(index) = expr.strip_prefix('x').and_then(|value| value.parse::<u8>().ok()) {
            return Ok(LoweringValue::Reg(ValueBinding { reg: index, width: 64 }));
        }
        if let Some(value) = parse_u64_literal(expr) {
            return Ok(LoweringValue::Imm(value));
        }
        bail!("lowering expression {expr} is not available in this translator context")
    }

    fn insert(&mut self, name: impl Into<String>, value: LoweringValue) {
        self.values.insert(name.into(), value);
    }

    fn llvm_key(&self, name: &str) -> Option<ValueKey> {
        self.llvm_value_keys.get(name).copied()
    }

    fn llvm_source_value(&self, name: &str) -> Option<BasicValueEnum<'ctx>> {
        self.llvm_sources.get(name).copied()
    }
}

#[derive(Debug, Default)]
struct ProfileInstructionArgs {
    values: HashMap<String, LoweringValue>,
}

impl ProfileInstructionArgs {
    fn from_emit(desc: &InstructionDesc, operands: &[(String, String)], env: &LoweringEnv<'_>) -> anyhow::Result<Self> {
        let mut values = HashMap::with_capacity(desc.operand_descs.len());
        for operand in &desc.operand_descs {
            let value = if let Some((_, expr)) = operands.iter().find(|(name, _)| name == &operand.name) {
                env.get(expr)?
            } else {
                env.get(&operand.name).with_context(|| {
                    format!(
                        "emit {} misses operand {} and translator context has no default",
                        desc.name, operand.name
                    )
                })?
            };
            values.insert(operand.name.clone(), value);
        }
        Ok(Self { values })
    }

    fn from_values(values: impl IntoIterator<Item = (String, LoweringValue)>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    fn raw(&self, name: &str) -> anyhow::Result<LoweringValue> {
        self.values
            .get(name)
            .copied()
            .with_context(|| format!("profile instruction operand {name} is missing"))
    }

    fn imm(&self, name: &str) -> anyhow::Result<u64> {
        match self.raw(name)? {
            LoweringValue::Imm(value) => Ok(value),
            LoweringValue::Reg(binding) => Ok(binding.reg as u64),
            LoweringValue::Label(_) => bail!("profile instruction operand {name} expected an immediate"),
        }
    }

    fn label(&self, name: &str) -> anyhow::Result<LabelId> {
        match self.raw(name)? {
            LoweringValue::Label(label) => Ok(label),
            LoweringValue::Reg(_) | LoweringValue::Imm(_) => {
                bail!("profile instruction operand {name} expected a label")
            },
        }
    }
}

fn operands_match_shape(operands: &[(String, String)], required: &[(&str, &str)]) -> bool {
    required.iter().all(|(name, expr)| {
        operands
            .iter()
            .any(|(actual_name, actual_expr)| actual_name == name && actual_expr == expr)
    })
}

impl<'m, 'ctx, 'profile> FunctionLowerer<'m, 'ctx, 'profile> {
    fn new(
        module: &'m mut Module<'ctx>,
        function: FunctionValue<'ctx>,
        name: &str,
        signature: &FunctionSignature,
        abi: &AbiProfile,
        lowering: &'profile LoweringProfile,
        isa: &'profile IsaProfile,
    ) -> anyhow::Result<Self> {
        let native_arg_registers = x_register_list("native_call args", &abi.native_args)?;
        let native_return_registers = x_register_list("native_call returns", &abi.native_returns)?;
        let native_clobber_registers = x_register_list("native_call clobbers", &abi.native_clobbers)?;
        let return_registers = (0..signature.return_slot_count())
            .map(|index| {
                abi.integer_returns
                    .get(index)
                    .copied()
                    .with_context(|| format!("profile ABI does not map return value {index}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_regs = signature
            .param_widths
            .iter()
            .enumerate()
            .map(|(index, _)| {
                abi.integer_args
                    .get(index)
                    .copied()
                    .with_context(|| format!("profile ABI does not map argument {index}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let reserved_vregs = param_regs
            .iter()
            .copied()
            .chain(return_registers.iter().copied())
            .chain(native_arg_registers.iter().copied())
            .chain(native_return_registers.iter().copied())
            .max()
            .map(|reg| reg + 1)
            .unwrap_or(0);
        let mut builder = VmFunctionBuilder::new(name, 0, signature.return_width);
        builder.reserve_vregs(reserved_vregs)?;
        let data_layout = module.get_data_layout();
        let layout = data_layout.as_str().to_string_lossy().into_owned();
        drop(data_layout);
        let target_data = TargetData::create(&layout);

        let values: HashMap<_, _> = function
            .get_params()
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                (
                    value_key(value),
                    ValueBinding {
                        reg: param_regs[index],
                        width: signature.param_widths[index],
                    },
                )
            })
            .collect();
        let defined_values = values.keys().copied().collect();
        let native_touched_registers = native_arg_registers
            .iter()
            .chain(native_return_registers.iter())
            .chain(native_clobber_registers.iter())
            .copied()
            .collect();

        Ok(Self {
            module,
            function,
            lowering,
            isa,
            target_data,
            builder,
            values,
            aggregates: HashMap::new(),
            labels: HashMap::new(),
            native_calls: Vec::new(),
            return_registers,
            native_arg_registers,
            native_return_registers,
            native_touched_registers,
            aggregate_return_fields: signature.aggregate_return_fields.clone(),
            defined_values,
            reuse_plan: None,
            temporary_regs: Vec::new(),
        })
    }

    fn lower(mut self) -> anyhow::Result<(VmFunction, Vec<NativeCallTarget<'ctx>>)> {
        let basic_blocks = self.function.get_basic_blocks();
        if basic_blocks.is_empty() {
            bail!("function has no basic blocks");
        }

        // LLVM CFG 边先变成 VM label。后续 branch/phi/select lowering 只引用 label，
        // bytecode encoder 再把 label 修正为指令 PC。
        for block in &basic_blocks {
            let label = self.builder.new_label();
            self.labels.insert(block_key(*block), label);
        }

        self.prepare_register_reuse(&basic_blocks)?;

        // phi 的语义通过 predecessor edge move 表达；phi 指令本身在 block 开头不 emit。
        for block in basic_blocks {
            let label = self
                .labels
                .get(&block_key(block))
                .copied()
                .context("missing VM label for basic block")?;
            self.builder.bind_label(label);
            self.lower_block(block)?;
        }

        Ok((self.builder.finish()?, self.native_calls))
    }

    fn prepare_register_reuse(&mut self, basic_blocks: &[BasicBlock<'ctx>]) -> anyhow::Result<()> {
        let plan = self.build_reuse_plan(basic_blocks)?;

        // pinned phi/result 需要在被引用前就拥有稳定寄存器，否则不同控制流路径会看到不同寄存器。
        for block in basic_blocks {
            for instruction in block.get_instructions() {
                let key = instruction_key(instruction);
                if instruction.get_opcode() == InstructionOpcode::Phi
                    && plan.pinned_values.contains(&key)
                    && !self.values.contains_key(&key)
                {
                    let width = instruction_result_width(instruction)?
                        .context("pinned VM value must have a scalar register width")?;
                    let reg = self.builder.alloc_vreg()?;
                    self.values.insert(key, ValueBinding { reg, width });
                }
            }
        }
        self.reuse_plan = Some(plan);
        Ok(())
    }

    fn build_reuse_plan(&self, basic_blocks: &[BasicBlock<'ctx>]) -> anyhow::Result<ReusePlan> {
        let mut value_blocks = HashMap::new();
        let mut result_values = HashSet::new();
        let mut pinned_values = self.values.keys().copied().collect::<HashSet<_>>();

        for block in basic_blocks {
            let block_key = block_key(*block);
            for instruction in block.get_instructions() {
                if instruction_result_width(instruction)?.is_some() {
                    let key = instruction_key(instruction);
                    value_blocks.insert(key, block_key);
                    result_values.insert(key);
                    if instruction.get_opcode() == InstructionOpcode::Phi {
                        pinned_values.insert(key);
                    }
                }
            }
        }

        // 跨 basic-block 或 phi edge 的值不能靠简单线性扫描回收：VM bytecode 可能绕过本应重新
        // 分配该寄存器的 block。固定这些值能让 allocator 保守，同时仍允许 block-local SSA 临时值密集复用。
        for block in basic_blocks {
            let user_block = block_key(*block);
            for instruction in block.get_instructions() {
                for operand in instruction_value_operands(instruction) {
                    let key = value_key(operand);
                    let Some(def_block) = value_blocks.get(&key).copied() else {
                        continue;
                    };
                    if instruction.get_opcode() == InstructionOpcode::Phi || def_block != user_block {
                        pinned_values.insert(key);
                    }
                }
            }
        }

        let mut block_last_uses = HashMap::new();
        for block in basic_blocks {
            let user_block = block_key(*block);
            let mut last_uses = HashMap::new();
            for (index, instruction) in block.get_instructions().enumerate() {
                if instruction.get_opcode() == InstructionOpcode::Phi {
                    continue;
                }
                for operand in instruction_value_operands(instruction) {
                    let key = value_key(operand);
                    if pinned_values.contains(&key) {
                        continue;
                    }
                    if result_values.contains(&key) && value_blocks.get(&key).copied() == Some(user_block) {
                        last_uses.insert(key, index);
                    }
                }
            }
            block_last_uses.insert(user_block, last_uses);
        }

        Ok(ReusePlan {
            pinned_values,
            block_last_uses,
            current_block: None,
            instruction_index: 0,
        })
    }

    fn begin_instruction(&mut self, _instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        Ok(())
    }

    fn ensure_result_binding(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<ValueBinding> {
        let key = instruction_key(instruction);
        if let Some(binding) = self.values.get(&key).copied() {
            return Ok(binding);
        }

        let width =
            instruction_result_width(instruction)?.context("instruction has no scalar result register width")?;
        let reg = self.builder.alloc_vreg()?;
        let binding = ValueBinding { reg, width };
        self.values.insert(key, binding);
        Ok(binding)
    }

    fn finish_instruction(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let result_key = instruction_key(instruction);
        if instruction_result_width(instruction)?.is_some() {
            self.defined_values.insert(result_key);
        }
        self.release_after_instruction(instruction)?;
        self.advance_reuse_plan();
        Ok(())
    }

    fn release_after_instruction(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let Some(plan) = self.reuse_plan.as_ref() else {
            return Ok(());
        };
        let instruction_index = plan.instruction_index;
        let last_uses = plan
            .current_block
            .and_then(|block| plan.block_last_uses.get(&block))
            .cloned()
            .unwrap_or_default();
        let pinned_values = plan.pinned_values.clone();

        let mut released = if instruction.get_opcode() == InstructionOpcode::Phi {
            Vec::new()
        } else {
            instruction_value_operands(instruction)
                .into_iter()
                .map(value_key)
                .filter(|key| !pinned_values.contains(key))
                .filter(|key| last_uses.get(key).copied() == Some(instruction_index))
                .collect::<Vec<_>>()
        };

        let result_key = instruction_key(instruction);
        if instruction_result_width(instruction)?.is_some()
            && !pinned_values.contains(&result_key)
            && !last_uses.contains_key(&result_key)
        {
            released.push(result_key);
        }

        self.release_instruction_temporaries();
        for key in released {
            if let Some(binding) = self.values.get(&key).copied() {
                self.builder.release_vreg(binding.reg);
            }
            self.defined_values.remove(&key);
            self.values.remove(&key);
        }

        Ok(())
    }

    fn advance_reuse_plan(&mut self) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.instruction_index += 1;
        }
    }

    fn alloc_temporary_vreg(&mut self) -> anyhow::Result<u8> {
        let reg = self.builder.alloc_vreg()?;
        if self.reuse_plan.is_some() {
            self.temporary_regs.push(reg);
        }
        Ok(reg)
    }

    fn alloc_temporary_vreg_excluding(&mut self, excluded: &HashSet<u8>) -> anyhow::Result<u8> {
        let reg = self.builder.alloc_vreg_excluding(excluded)?;
        if self.reuse_plan.is_some() {
            self.temporary_regs.push(reg);
        }
        Ok(reg)
    }

    fn release_instruction_temporaries(&mut self) {
        for reg in self.temporary_regs.drain(..) {
            self.builder.release_vreg(reg);
        }
    }

    fn begin_block(&mut self, block: BasicBlock<'ctx>) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.current_block = Some(block_key(block));
            plan.instruction_index = 0;
        }
    }

    fn end_block(&mut self) {
        if let Some(plan) = self.reuse_plan.as_mut() {
            plan.current_block = None;
            plan.instruction_index = 0;
        }
        self.release_instruction_temporaries();
    }

    fn lower_block(&mut self, block: BasicBlock<'ctx>) -> anyhow::Result<()> {
        self.begin_block(block);
        for instruction in block.get_instructions() {
            self.begin_instruction(instruction)?;
            match instruction.get_opcode() {
                InstructionOpcode::Phi => {},
                InstructionOpcode::Add
                | InstructionOpcode::Sub
                | InstructionOpcode::Mul
                | InstructionOpcode::Xor
                | InstructionOpcode::And
                | InstructionOpcode::Or
                | InstructionOpcode::Shl
                | InstructionOpcode::LShr
                | InstructionOpcode::AShr => self.lower_binop(instruction)?,
                InstructionOpcode::ICmp => self.lower_icmp(instruction)?,
                InstructionOpcode::ZExt
                | InstructionOpcode::SExt
                | InstructionOpcode::Trunc
                | InstructionOpcode::BitCast
                | InstructionOpcode::PtrToInt
                | InstructionOpcode::IntToPtr => self.lower_cast(instruction)?,
                InstructionOpcode::Alloca => self.lower_alloca(instruction)?,
                InstructionOpcode::Load => self.lower_load(instruction)?,
                InstructionOpcode::Store => self.lower_store(instruction)?,
                InstructionOpcode::GetElementPtr => self.lower_gep(instruction)?,
                InstructionOpcode::Call => self.lower_call(instruction)?,
                InstructionOpcode::Select => self.lower_select(instruction)?,
                InstructionOpcode::InsertValue => self.lower_insert_value(instruction)?,
                InstructionOpcode::ExtractValue => self.lower_extract_value(instruction)?,
                InstructionOpcode::Br => self.lower_branch(block, instruction)?,
                InstructionOpcode::Switch => self.lower_switch(block, instruction)?,
                InstructionOpcode::Return => self.lower_return(instruction)?,
                opcode => bail!("unsupported instruction opcode: {opcode:?}"),
            }
            self.finish_instruction(instruction)?;
        }
        self.end_block();

        Ok(())
    }

    fn lowering_rule(&self, contract: &str) -> anyhow::Result<&LoweringRule> {
        let pattern = lowering_match_pattern(contract)
            .with_context(|| format!("translator requested unknown lowering contract {contract}"))?;
        self.lowering
            .rule_by_match(pattern)
            .ok_or_else(|| anyhow::anyhow!("profile lowering does not declare {contract} match {pattern}"))
    }

    fn instruction_desc(&self, name: &str) -> anyhow::Result<&InstructionDesc> {
        self.isa
            .instructions
            .iter()
            .find(|instruction| instruction.name == name)
            .ok_or_else(|| anyhow::anyhow!("profile ISA does not declare instruction {name}"))
    }

    fn instruction_desc_for_semantic(&self, semantic: &HandlerSemantic) -> anyhow::Result<InstructionDesc> {
        let mut matches = self
            .isa
            .instructions
            .iter()
            .filter(|instruction| instruction.semantic == *semantic);
        let first = matches
            .next()
            .cloned()
            .with_context(|| format!("profile ISA does not declare semantic {semantic:?}"))?;
        if let Some(second) = matches.next() {
            bail!(
                "profile ISA semantic {semantic:?} is ambiguous between {} and {}; use a lowering emit instruction name",
                first.name,
                second.name
            );
        }
        Ok(first)
    }

    fn emit_action_for_shape(
        &self,
        rule: &str,
        semantic: &HandlerSemantic,
        required_operands: &[(&str, &str)],
    ) -> anyhow::Result<LoweringAction> {
        let mut matches = self
            .lowering_rule(rule)?
            .actions
            .iter()
            .filter_map(|action| match action {
                LoweringAction::Emit { instruction, operands } => {
                    let desc = self.instruction_desc(instruction).ok()?;
                    (desc.semantic == *semantic && operands_match_shape(operands, required_operands))
                        .then_some(action.clone())
                },
                _ => None,
            })
            .collect::<Vec<_>>();

        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => bail!("profile lowering rule {rule} does not emit {semantic:?} with required operand shape"),
            count => bail!("profile lowering rule {rule} has {count} emits matching {semantic:?} and operand shape"),
        }
    }

    fn emit_profile_action(&mut self, action: &LoweringAction, env: &LoweringEnv<'ctx>) -> anyhow::Result<()> {
        let LoweringAction::Emit { instruction, operands } = action else {
            bail!("lowering action is not an emit action");
        };
        let desc = self.instruction_desc(instruction)?.clone();
        let args = ProfileInstructionArgs::from_emit(&desc, operands, env)?;
        self.emit_profile_instruction(&desc, args)
            .with_context(|| format!("while emitting profile instruction {}", desc.name))
    }

    fn execute_lowering_rule(
        &mut self,
        rule: &str,
        mut env: LoweringEnv<'ctx>,
        selected_semantic: Option<HandlerSemantic>,
    ) -> anyhow::Result<LoweringEnv<'ctx>> {
        let actions = self.lowering_rule(rule)?.actions.clone();
        let mut emitted = 0_usize;

        // lowering.vm 的 action 顺序是语义的一部分：
        // materialize/vreg 先准备 VM 值，emit 产生 profile 指令，bind 再把 LLVM 结果挂到 VM 寄存器。
        for action in &actions {
            match action {
                LoweringAction::Materialize {
                    target,
                    source,
                    value_type,
                } => {
                    let value =
                        self.materialize_lowering_value(source, value_type.as_deref().unwrap_or("i64"), &env)?;
                    env.insert(target.clone(), LoweringValue::Reg(value));
                },
                LoweringAction::VReg { target, value_type } => {
                    if env.get(target).is_ok() {
                        continue;
                    }
                    let reg = self.builder.alloc_vreg()?;
                    let width = width_from_lowering_value_type(value_type, &env)?;
                    env.insert(target.clone(), LoweringValue::Reg(ValueBinding { reg, width }));
                },
                LoweringAction::Emit { instruction, .. } => {
                    let desc = self.instruction_desc(instruction)?;
                    if selected_semantic
                        .as_ref()
                        .is_some_and(|semantic| desc.semantic != *semantic)
                    {
                        continue;
                    }
                    self.emit_profile_action(action, &env)?;
                    emitted += 1;
                },
                LoweringAction::Bind { llvm_value, vm_value } => {
                    let LoweringValue::Reg(binding) = env.get(vm_value)? else {
                        bail!("lowering bind {llvm_value} = {vm_value} expected a VM register");
                    };
                    env.insert(llvm_value.clone(), LoweringValue::Reg(binding));
                    if let Some(key) = env.llvm_key(llvm_value) {
                        self.values.insert(key, binding);
                    }
                },
            }
        }

        if emitted == 0 {
            if let Some(semantic) = selected_semantic {
                bail!("profile lowering rule {rule} did not emit semantic {semantic:?}");
            }
            bail!("profile lowering rule {rule} did not emit any VM instruction");
        }

        Ok(env)
    }

    fn materialize_lowering_value(
        &mut self,
        source: &str,
        value_type: &str,
        env: &LoweringEnv<'ctx>,
    ) -> anyhow::Result<ValueBinding> {
        let value = match env.get(source) {
            Ok(value) => value,
            Err(error) => {
                if let Some(llvm_value) = env.llvm_source_value(source) {
                    return self.materialize_value(llvm_value);
                }
                return Err(error);
            },
        };

        match value {
            LoweringValue::Reg(binding) => Ok(binding),
            LoweringValue::Imm(value) => {
                let width = width_from_lowering_value_type(value_type, env)?;
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, value, width)?;
                Ok(ValueBinding { reg, width })
            },
            LoweringValue::Label(_) => bail!("lowering materialize {source} cannot materialize a label"),
        }
    }

    fn emit_profile_instruction(&mut self, desc: &InstructionDesc, args: ProfileInstructionArgs) -> anyhow::Result<()> {
        // 这里是 profile semantic AST 到内部 VmInstruction 的最后一跳。内部 enum 只保存语义操作，
        // 但 push_profile 会额外记录 desc.name，避免同语义指令在 bytecode 阶段丢失 profile identity。
        match &desc.semantic {
            HandlerSemantic::MovImm => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let imm = args.imm("imm")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::MovImm { dst, imm, width }, desc.name.clone());
            },
            HandlerSemantic::ConstLoad => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let value = args.imm("index")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::ConstLoad { dst, value, width }, desc.name.clone());
            },
            HandlerSemantic::Mov => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Mov { dst, src, width }, desc.name.clone());
            },
            HandlerSemantic::Bin(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::Bin {
                        op: *op,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Icmp => {
                let pred = predicate_from_u64(args.imm("pred")?)?;
                let dst = self.profile_reg(desc, &args, "dst")?;
                let lhs = self.profile_reg(desc, &args, "lhs")?;
                let rhs = self.profile_reg(desc, &args, "rhs")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder.push_profile(
                    VmInstruction::Icmp {
                        pred,
                        dst,
                        lhs,
                        rhs,
                        width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Cast(op) => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let src = self.profile_reg(desc, &args, "src")?;
                let from_width = checked_width_u64(args.imm("from_width")?)?;
                let to_width = checked_width_u64(args.imm("to_width")?)?;
                self.builder.push_profile(
                    VmInstruction::Cast {
                        op: *op,
                        dst,
                        src,
                        from_width,
                        to_width,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Alloca => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let bytes = args.imm("bytes")?;
                let align = u8::try_from(args.imm("align")?).context("alloca align does not fit in u8")?;
                self.builder
                    .push_profile(VmInstruction::Alloca { dst, bytes, align }, desc.name.clone());
            },
            HandlerSemantic::Load => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Load { dst, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::Store => {
                let src = self.profile_reg(desc, &args, "src")?;
                let ptr = self.profile_reg(desc, &args, "ptr")?;
                let width = checked_width_u64(args.imm("width")?)?;
                self.builder
                    .push_profile(VmInstruction::Store { src, ptr, width }, desc.name.clone());
            },
            HandlerSemantic::Gep => {
                let dst = self.profile_reg(desc, &args, "dst")?;
                let base = self.profile_reg(desc, &args, "base")?;
                let offset = args.imm("offset")?;
                self.builder
                    .push_profile(VmInstruction::Gep { dst, base, offset }, desc.name.clone());
            },
            HandlerSemantic::Br => {
                let target = args.label("target")?;
                self.builder
                    .push_profile(VmInstruction::Br { target }, desc.name.clone());
            },
            HandlerSemantic::BrCond => {
                let cond = self.profile_reg(desc, &args, "cond")?;
                let then_label = args.label("then_pc")?;
                let else_label = args.label("else_pc")?;
                self.builder.push_profile(
                    VmInstruction::BrCond {
                        cond,
                        then_label,
                        else_label,
                    },
                    desc.name.clone(),
                );
            },
            HandlerSemantic::Ret => {
                let src = self.profile_reg(desc, &args, "src")?;
                self.builder.push_profile(VmInstruction::Ret { src }, desc.name.clone());
            },
            HandlerSemantic::CallNative | HandlerSemantic::Nop | HandlerSemantic::VmCall | HandlerSemantic::VmRet => {
                if desc.semantic != HandlerSemantic::CallNative {
                    bail!(
                        "profile instruction {} is not supported by the LLVM translator emitter",
                        desc.name
                    );
                }
                let call_id = u16::try_from(args.imm("callee")?)
                    .with_context(|| format!("call_native callee operand in {} does not fit in u16", desc.name))?;
                let argc = usize::try_from(args.imm("argc")?)
                    .with_context(|| format!("call_native argc operand in {} does not fit in usize", desc.name))?;
                if argc > NATIVE_CALL_MAX_ARGS {
                    bail!("call_native supports at most {NATIVE_CALL_MAX_ARGS} arguments, got {argc}");
                }
                let ret_count = usize::try_from(args.imm("ret_count")?)
                    .with_context(|| format!("call_native ret_count operand in {} does not fit in usize", desc.name))?;
                if ret_count > NATIVE_CALL_MAX_RETURNS {
                    bail!("call_native supports at most {NATIVE_CALL_MAX_RETURNS} returns, got {ret_count}");
                }

                let mut call_args = Vec::with_capacity(argc);
                for index in 0..argc {
                    call_args.push(self.profile_reg(desc, &args, &format!("arg{index}"))?);
                }

                let mut returns = Vec::with_capacity(ret_count);
                for index in 0..ret_count {
                    returns.push(NativeReturn {
                        dst: self.profile_reg(desc, &args, &format!("ret{index}"))?,
                        width: checked_width_u64(args.imm(&format!("ret{index}_width"))?)?,
                    });
                }

                self.builder.push_profile(
                    VmInstruction::CallNative {
                        call_id,
                        args: call_args,
                        returns,
                    },
                    desc.name.clone(),
                );
            },
        }
        Ok(())
    }

    fn emit_profile_mov_imm(&mut self, dst: u8, imm: u64, width: u8) -> anyhow::Result<()> {
        let desc = self.instruction_desc_for_semantic(&HandlerSemantic::MovImm)?;
        let args = ProfileInstructionArgs::from_values([
            ("dst".to_owned(), LoweringValue::Reg(ValueBinding { reg: dst, width })),
            ("imm".to_owned(), LoweringValue::Imm(imm)),
            ("width".to_owned(), LoweringValue::Imm(width as u64)),
        ]);
        self.emit_profile_instruction(&desc, args)
    }

    fn emit_profile_mov_direct(&mut self, dst: u8, src: u8, width: u8) -> anyhow::Result<()> {
        let desc = self.instruction_desc_for_semantic(&HandlerSemantic::Mov)?;
        let args = ProfileInstructionArgs::from_values([
            ("dst".to_owned(), LoweringValue::Reg(ValueBinding { reg: dst, width })),
            ("src".to_owned(), LoweringValue::Reg(ValueBinding { reg: src, width })),
            ("width".to_owned(), LoweringValue::Imm(width as u64)),
        ]);
        self.emit_profile_instruction(&desc, args)
    }

    fn profile_reg(&mut self, desc: &InstructionDesc, args: &ProfileInstructionArgs, name: &str) -> anyhow::Result<u8> {
        let operand = desc
            .operand_descs
            .iter()
            .find(|operand| operand.name == name)
            .with_context(|| format!("profile instruction {} has no operand {name}", desc.name))?;
        match args.raw(name)? {
            LoweringValue::Reg(binding) => Ok(binding.reg),
            LoweringValue::Imm(value) if operand.kind == OperandKind::VReg => {
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, value, width_from_operand_type(&operand.value_type))?;
                Ok(reg)
            },
            LoweringValue::Imm(_) | LoweringValue::Label(_) => {
                bail!("profile instruction operand {name} expected an x register")
            },
        }
    }

    fn lower_binop(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let (rule, selected) = match instruction.get_opcode() {
            InstructionOpcode::Add => ("llvm.add.integer", None),
            InstructionOpcode::Sub => ("llvm.sub.integer", None),
            InstructionOpcode::Mul => ("llvm.mul.integer", None),
            InstructionOpcode::Xor => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::Xor))),
            InstructionOpcode::And => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::And))),
            InstructionOpcode::Or => ("llvm.bitops.integer", Some(HandlerSemantic::Bin(BinOp::Or))),
            InstructionOpcode::Shl => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::Shl))),
            InstructionOpcode::LShr => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::LShr))),
            InstructionOpcode::AShr => ("llvm.shift.integer", Some(HandlerSemantic::Bin(BinOp::AShr))),
            opcode => bail!("unsupported binop opcode: {opcode:?}"),
        };
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let width = instruction_result_width(instruction)?.context("binop result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%r)", width as u64);
        self.execute_lowering_rule(rule, env, selected)?;
        Ok(())
    }

    fn lower_alloca(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let allocated_type = instruction
            .get_allocated_type()
            .map_err(|err| anyhow::anyhow!("failed to read alloca type: {err}"))?;
        let element_size = self.target_data.get_store_size(&allocated_type);
        if element_size == 0 {
            bail!("zero-sized alloca is not supported");
        }
        let count = self.static_alloca_count(instruction)?;
        let bytes = element_size.checked_mul(count).context("alloca byte size overflow")?;
        let align = instruction
            .get_alignment()
            .ok()
            .and_then(|align| u8::try_from(align).ok())
            .unwrap_or(1);

        let env = LoweringEnv::new()
            .llvm_value("%r", instruction_key(instruction))
            .imm("alloc_size(%ty)", bytes)
            .imm("alloc_align(%r)", align as u64);
        self.execute_lowering_rule("llvm.alloca.stack", env, Some(HandlerSemantic::Alloca))?;
        Ok(())
    }

    fn lower_load(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let ptr = instruction_operand_value(instruction, 0)?;
        let width = instruction_result_width(instruction)?.context("load result has no scalar width")?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .llvm_value("%r", instruction_key(instruction))
            .imm("memory_width(%ptr)", width as u64);
        self.execute_lowering_rule("llvm.memory.scalar", env, Some(HandlerSemantic::Load))?;
        Ok(())
    }

    fn lower_store(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = self.materialize_operand(instruction, 0)?;
        let ptr = instruction_operand_value(instruction, 1)?;
        let env = LoweringEnv::new()
            .llvm_source("%ptr", ptr)
            .binding("%value", src)
            .binding("%vv", src)
            .imm("memory_width(%ptr)", src.width as u64);
        self.execute_lowering_rule("llvm.memory.scalar", env, Some(HandlerSemantic::Store))?;
        Ok(())
    }

    fn lower_gep(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let gep = GepInst::new(instruction);
        let base_value = gep
            .get_pointer_operand()
            .context("getelementptr has no pointer operand")?;
        let Some(offset) = gep.accumulate_constant_offset(self.module) else {
            let binding = self.ensure_result_binding(instruction)?;
            let base = self.materialize_value(base_value)?;
            return self.lower_dynamic_gep(instruction, gep, binding, base);
        };
        let env = LoweringEnv::new()
            .llvm_source("%base", base_value)
            .llvm_value("%r", instruction_key(instruction))
            .imm("constant_gep_offset(%r)", offset as u64);
        self.execute_lowering_rule("llvm.gep.constant", env, Some(HandlerSemantic::Gep))?;
        Ok(())
    }

    fn lower_dynamic_gep(
        &mut self,
        instruction: InstructionValue<'ctx>,
        gep: GepInst<'ctx>,
        dst: ValueBinding,
        base: ValueBinding,
    ) -> anyhow::Result<()> {
        let terms = self.gep_dynamic_terms(instruction, gep)?;
        let mut address = base.reg;

        // dynamic GEP 被拆成：base + sum(index_i * element_size_i) + constant_offset。
        // 每个乘加都必须通过 profile 中的 llvm.gep.dynamic rule emit，不能在这里绕过 ISA。
        if terms.dynamic.is_empty() {
            if address != dst.reg {
                let action = self.emit_action_for_shape(
                    "llvm.phi.edge_move",
                    &HandlerSemantic::Mov,
                    &[("dst", "%vr"), ("src", "%vi"), ("width", "type_width(%r)")],
                )?;
                let env = LoweringEnv::new()
                    .reg("%vr", dst.reg, 64)
                    .reg("%vi", address, 64)
                    .imm("type_width(%r)", 64);
                self.emit_profile_action(&action, &env)?;
            }
            return Ok(());
        }

        let dynamic_len = terms.dynamic.len();
        let mul_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Bin(BinOp::Mul),
            &[
                ("dst", "%vs"),
                ("lhs", "%vi"),
                ("rhs", "element_size(%base)"),
                ("width", "64"),
            ],
        )?;
        let add_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Bin(BinOp::Add),
            &[("dst", "%vr"), ("lhs", "%vb"), ("rhs", "%vs"), ("width", "64")],
        )?;
        let gep_action = self.emit_action_for_shape(
            "llvm.gep.dynamic",
            &HandlerSemantic::Gep,
            &[("dst", "%vr"), ("base", "%vr"), ("offset", "constant_gep_offset(%r)")],
        )?;
        for (term_index, (index, scale)) in terms.dynamic.into_iter().enumerate() {
            let scale_reg = self.alloc_temporary_vreg()?;
            self.push_constant(scale_reg, scale, 64)?;
            let scaled = self.alloc_temporary_vreg()?;
            let mul_env = LoweringEnv::new()
                .binding("%vi", index)
                .reg("%vs", scaled, 64)
                .reg("element_size(%base)", scale_reg, 64)
                .imm("64", 64);
            self.emit_profile_action(&mul_action, &mul_env)?;
            let is_last = term_index + 1 == dynamic_len;
            let sum = if is_last { dst.reg } else { self.alloc_temporary_vreg()? };
            let add_env = LoweringEnv::new()
                .reg("%vb", address, 64)
                .reg("%vs", scaled, 64)
                .reg("%vr", sum, 64)
                .imm("64", 64);
            self.emit_profile_action(&add_action, &add_env)?;
            address = sum;
        }
        if terms.constant_offset != 0 {
            let env = LoweringEnv::new()
                .reg("%vr", dst.reg, 64)
                .reg("%vb", address, 64)
                .imm("constant_gep_offset(%r)", terms.constant_offset as u64);
            self.emit_profile_action(&gep_action, &env)?;
        }
        Ok(())
    }

    fn lower_call(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let call_action = self.emit_action_for_shape(
            "llvm.call.direct",
            &HandlerSemantic::CallNative,
            &[
                ("argc", "arg_count(%callee)"),
                ("arg0", "arg0"),
                ("ret_count", "return_count(%callee)"),
            ],
        )?;
        let call = CallInst::new(instruction);
        let callee = call
            .get_call_function()
            .context("indirect calls are not supported by vm_virtualize")?;
        if callee.get_intrinsic_id() != 0 || callee.is_llvm_function() {
            bail!("LLVM intrinsic calls are not supported by vm_virtualize");
        }

        let target = native_call_target(callee)?;
        if target.param_widths.len() > self.native_arg_registers.len() {
            bail!(
                "profile native_call ABI maps {} argument registers but callee needs {}",
                self.native_arg_registers.len(),
                target.param_widths.len()
            );
        }
        if target.return_fields.len() > self.native_return_registers.len() {
            bail!(
                "profile native_call ABI maps {} return registers but callee needs {}",
                self.native_return_registers.len(),
                target.return_fields.len()
            );
        }

        let final_returns = self.native_call_final_returns(instruction, &target)?;
        let result_regs = final_returns.iter().map(|binding| binding.reg).collect::<HashSet<_>>();

        // call_native 的 VM ABI 是 profile 固定的 x 寄存器列表；真实 LLVM callee 类型由 thunk 恢复。
        // 因此这里先把每个 operand materialize，再并行移动到 profile 指定的 call argument 寄存器。
        let args = (0..target.param_widths.len())
            .map(|index| {
                let value = self.materialize_operand(instruction, index as u32)?;
                if value.width != target.param_widths[index] {
                    bail!(
                        "native call argument {index} width mismatch: value is {}, callee expects {}",
                        value.width,
                        target.param_widths[index]
                    );
                }
                Ok(value)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        // `native_call` 是 profile 声明的 ABI 边界。把参数移动到 call register 可能覆盖无关的
        // VM SSA 值，因此 translator 会保存 profile 声明 native bridge 可能触碰的所有已定义 x
        // 寄存器，但排除此调用结果值拥有的寄存器。
        let saved = self.save_native_touched_registers(&result_regs)?;

        let arg_moves = args
            .iter()
            .enumerate()
            .map(|(index, value)| RegisterMove {
                dst: self.native_arg_registers[index],
                src: *value,
            })
            .collect::<Vec<_>>();
        self.emit_parallel_register_moves(arg_moves)?;

        let call_id = u16::try_from(self.native_calls.len()).context("native call table has too many entries")?;
        let native_returns = target
            .return_fields
            .iter()
            .enumerate()
            .map(|(index, field)| NativeReturn {
                dst: self.native_return_registers[index],
                width: field.width,
            })
            .collect::<Vec<_>>();
        self.native_calls.push(target);
        let mut env = LoweringEnv::new()
            .imm("native_id(%callee)", call_id as u64)
            .imm("callee", call_id as u64)
            .imm("arg_count(%callee)", args.len() as u64)
            .imm("argc", args.len() as u64)
            .imm("return_count(%callee)", native_returns.len() as u64)
            .imm("ret_count", native_returns.len() as u64);
        for index in 0..NATIVE_CALL_MAX_ARGS {
            let reg = self.native_arg_registers.get(index).copied().unwrap_or(0);
            env = env.reg(format!("arg{index}"), reg, 64);
        }
        for index in 0..NATIVE_CALL_MAX_RETURNS {
            let ret = native_returns
                .get(index)
                .copied()
                .unwrap_or(NativeReturn { dst: 0, width: 64 });
            env = env
                .reg(format!("ret{index}"), ret.dst, ret.width)
                .imm(format!("ret{index}_width"), ret.width as u64);
        }
        self.emit_profile_action(&call_action, &env)?;

        for (native, final_return) in native_returns.iter().zip(final_returns.iter()) {
            if native.dst != final_return.reg {
                self.emit_profile_mov_direct(final_return.reg, native.dst, final_return.width)?;
            }
        }

        if final_returns.len() > 1 {
            self.aggregates.insert(
                instruction_key(instruction),
                AggregateBinding {
                    fields: final_returns.into_iter().map(Some).collect(),
                },
            );
        }
        self.restore_native_touched_registers(saved, &result_regs)?;
        Ok(())
    }

    fn native_call_final_returns(
        &mut self,
        instruction: InstructionValue<'ctx>,
        target: &NativeCallTarget<'ctx>,
    ) -> anyhow::Result<Vec<ValueBinding>> {
        if target.returns_void {
            return Ok(Vec::new());
        }

        if target.return_fields.len() == 1 {
            let dst = self.ensure_result_binding(instruction)?;
            let field = target.return_fields[0];
            if field.width != dst.width {
                bail!(
                    "native call return width mismatch: destination is {}, callee returns {}",
                    dst.width,
                    field.width
                );
            }
            return Ok(vec![dst]);
        }

        target
            .return_fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let reg = self
                    .builder
                    .alloc_vreg_excluding(&self.native_touched_registers)
                    .with_context(|| format!("native aggregate return field {index}"))?;
                Ok(ValueBinding {
                    reg,
                    width: field.width,
                })
            })
            .collect()
    }

    fn save_native_touched_registers(&mut self, result_regs: &HashSet<u8>) -> anyhow::Result<Vec<(u8, u8)>> {
        let touched = self.native_touched_registers.clone();
        let mut seen = HashSet::new();
        let mut saves = Vec::new();
        let candidates = self
            .values
            .iter()
            .filter_map(|(key, binding)| {
                (self.defined_values.contains(key)
                    && !result_regs.contains(&binding.reg)
                    && touched.contains(&binding.reg)
                    && seen.insert(binding.reg))
                .then_some(binding.reg)
            })
            .collect::<Vec<_>>();

        for reg in candidates {
            let scratch = self.alloc_temporary_vreg_excluding(&touched)?;
            self.emit_profile_mov_direct(scratch, reg, 64)?;
            saves.push((reg, scratch));
        }

        Ok(saves)
    }

    fn emit_parallel_register_moves(&mut self, moves: Vec<RegisterMove>) -> anyhow::Result<()> {
        // 参数搬运可能形成 x1->x2、x2->x1 这种环。先把会被覆盖的源寄存器复制到 scratch，
        // 再执行目标写入，避免 native_call 参数准备阶段破坏尚未移动的源值。
        let clobbered = moves
            .iter()
            .filter(|mov| mov.dst != mov.src.reg)
            .map(|mov| mov.dst)
            .collect::<HashSet<_>>();
        let mut prepared = Vec::new();

        for mov in moves {
            if mov.dst == mov.src.reg {
                continue;
            }

            let src = if clobbered.contains(&mov.src.reg) {
                let scratch = self.alloc_temporary_vreg_excluding(&clobbered)?;
                self.emit_profile_mov_direct(scratch, mov.src.reg, mov.src.width)?;
                scratch
            } else {
                mov.src.reg
            };
            prepared.push((mov.dst, src, mov.src.width));
        }

        for (dst, src, width) in prepared {
            self.emit_profile_mov_direct(dst, src, width)?;
        }

        Ok(())
    }

    fn restore_native_touched_registers(
        &mut self,
        saves: Vec<(u8, u8)>,
        result_regs: &HashSet<u8>,
    ) -> anyhow::Result<()> {
        for (reg, scratch) in saves {
            if !result_regs.contains(&reg) {
                self.emit_profile_mov_direct(reg, scratch, 64)?;
            }
        }
        Ok(())
    }

    fn lower_icmp(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let lhs = instruction_operand_value(instruction, 0)?;
        let rhs = instruction_operand_value(instruction, 1)?;
        let pred = instruction
            .get_icmp_predicate()
            .context("icmp instruction has no predicate")?;
        let lhs_width = value_width(lhs)?;
        let rhs_width = value_width(rhs)?;

        let env = LoweringEnv::new()
            .llvm_source("%a", lhs)
            .llvm_source("%b", rhs)
            .llvm_value("%r", instruction_key(instruction))
            .imm("predicate(%r)", map_predicate(pred) as u64)
            .imm("operand_width(%a,%b)", lhs_width.max(rhs_width) as u64);
        self.execute_lowering_rule("llvm.icmp.integer", env, Some(HandlerSemantic::Icmp))?;
        Ok(())
    }

    fn lower_cast(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let src = instruction_operand_value(instruction, 0)?;
        let src_width = value_width(src)?;
        let dst_width = instruction_result_width(instruction)?.context("cast result has no scalar width")?;
        let (rule, required) = match instruction.get_opcode() {
            InstructionOpcode::ZExt => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::ZExt)),
            InstructionOpcode::SExt => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::SExt)),
            InstructionOpcode::Trunc => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::Trunc)),
            InstructionOpcode::BitCast => ("llvm.cast.integer", HandlerSemantic::Cast(CastOp::Bitcast)),
            InstructionOpcode::PtrToInt => {
                if dst_width < src_width {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Trunc))
                } else {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Bitcast))
                }
            },
            InstructionOpcode::IntToPtr => {
                if src_width < dst_width {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::ZExt))
                } else {
                    ("llvm.cast.pointer", HandlerSemantic::Cast(CastOp::Bitcast))
                }
            },
            opcode => bail!("unsupported cast opcode: {opcode:?}"),
        };
        let env = LoweringEnv::new()
            .llvm_source("%a", src)
            .llvm_value("%r", instruction_key(instruction))
            .imm("type_width(%a)", src_width as u64)
            .imm("type_width(%r)", dst_width as u64);
        self.execute_lowering_rule(rule, env, Some(required))?;
        Ok(())
    }

    fn lower_select(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let dst = self.ensure_result_binding(instruction)?;
        let cond = self.materialize_operand(instruction, 0)?;
        let then_value = self.materialize_operand(instruction, 1)?;
        let else_value = self.materialize_operand(instruction, 2)?;
        let then_label = self.builder.new_label();
        let else_label = self.builder.new_label();
        let join_label = self.builder.new_label();
        let br_if = self.emit_action_for_shape(
            "llvm.select.integer",
            &HandlerSemantic::BrCond,
            &[("cond", "%vc"), ("then_pc", "then_label"), ("else_pc", "else_label")],
        )?;
        let then_mov = self.emit_action_for_shape(
            "llvm.select.integer",
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%vt"), ("width", "type_width(%r)")],
        )?;
        let then_br =
            self.emit_action_for_shape("llvm.select.integer", &HandlerSemantic::Br, &[("target", "join_label")])?;
        let else_mov = self.emit_action_for_shape(
            "llvm.select.integer",
            &HandlerSemantic::Mov,
            &[("dst", "%vr"), ("src", "%ve"), ("width", "type_width(%r)")],
        )?;

        let branch_env = LoweringEnv::new()
            .binding("%vc", cond)
            .label("then_label", then_label)
            .label("else_label", else_label);
        self.emit_profile_action(&br_if, &branch_env)?;

        self.builder.bind_label(then_label);
        let then_env = LoweringEnv::new()
            .binding("%vr", dst)
            .binding("%vt", then_value)
            .imm("type_width(%r)", dst.width as u64)
            .label("join_label", join_label);
        self.emit_profile_action(&then_mov, &then_env)?;
        self.emit_profile_action(&then_br, &then_env)?;

        self.builder.bind_label(else_label);
        let else_env = LoweringEnv::new()
            .binding("%vr", dst)
            .binding("%ve", else_value)
            .imm("type_width(%r)", dst.width as u64);
        self.emit_profile_action(&else_mov, &else_env)?;
        self.emit_profile_action(&then_br, &then_env)?;

        self.builder.bind_label(join_label);
        Ok(())
    }

    fn lower_insert_value(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let field_index = aggregate_single_index(instruction)?;
        let mut aggregate = self.aggregate_seed_from_operand(instruction, 0)?;
        let inserted = instruction_operand_value(instruction, 1)?;
        let inserted_width = value_width(inserted)?;
        let env = LoweringEnv::new()
            .llvm_source("%field", inserted)
            .imm("type_width(%field)", inserted_width as u64);
        let env = self.execute_lowering_rule("llvm.aggregate.insert", env, Some(HandlerSemantic::Mov))?;
        let stable = match env.get("%r")? {
            LoweringValue::Reg(binding) => binding,
            LoweringValue::Imm(_) | LoweringValue::Label(_) => bail!("aggregate insert bind must produce a register"),
        };
        let slot = aggregate
            .fields
            .get_mut(field_index)
            .with_context(|| format!("insertvalue field {field_index} is out of range"))?;
        *slot = Some(stable);
        self.aggregates.insert(instruction_key(instruction), aggregate);
        Ok(())
    }

    fn lower_extract_value(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let field_index = aggregate_single_index(instruction)?;
        let aggregate = self.aggregate_operand(instruction, 0)?;
        let src = aggregate
            .fields
            .get(field_index)
            .copied()
            .flatten()
            .with_context(|| format!("extractvalue field {field_index} is out of range"))?;

        let result_width = instruction_result_width(instruction)?.context("extractvalue result has no scalar width")?;
        let env = LoweringEnv::new()
            .binding("%agg", src)
            .llvm_value("%r", instruction_key(instruction))
            .binding("field(%va)", src)
            .imm("type_width(%r)", result_width as u64);
        self.execute_lowering_rule("llvm.aggregate.extract", env, Some(HandlerSemantic::Mov))?;
        Ok(())
    }

    fn lower_branch(&mut self, block: BasicBlock<'ctx>, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let branch = instruction.into_branch_inst();
        if instruction.get_num_operands() == 1 {
            let target = branch
                .get_successor(0)
                .context("unconditional branch has no successor")?;
            self.lower_phi_moves(block, target)?;
            let action =
                self.emit_action_for_shape("llvm.br.control", &HandlerSemantic::Br, &[("target", "target_label")])?;
            let env = LoweringEnv::new().label("target_label", self.label_for_block(target)?);
            self.emit_profile_action(&action, &env)?;
            return Ok(());
        }

        let cond = self.materialize_operand(instruction, 0)?;
        let then_block = branch
            .get_successor(0)
            .context("conditional branch has no then successor")?;
        let else_block = branch
            .get_successor(1)
            .context("conditional branch has no else successor")?;
        let then_edge = self.builder.new_label();
        let else_edge = self.builder.new_label();
        let cond_action = self.emit_action_for_shape(
            "llvm.br.control",
            &HandlerSemantic::BrCond,
            &[("cond", "%vc"), ("then_pc", "then_label"), ("else_pc", "else_label")],
        )?;
        let br_action =
            self.emit_action_for_shape("llvm.br.control", &HandlerSemantic::Br, &[("target", "target_label")])?;

        let cond_env = LoweringEnv::new()
            .binding("%vc", cond)
            .label("then_label", then_edge)
            .label("else_label", else_edge);
        self.emit_profile_action(&cond_action, &cond_env)?;

        self.builder.bind_label(then_edge);
        self.lower_phi_moves(block, then_block)?;
        let then_env = LoweringEnv::new().label("target_label", self.label_for_block(then_block)?);
        self.emit_profile_action(&br_action, &then_env)?;

        self.builder.bind_label(else_edge);
        self.lower_phi_moves(block, else_block)?;
        let else_env = LoweringEnv::new().label("target_label", self.label_for_block(else_block)?);
        self.emit_profile_action(&br_action, &else_env)?;

        Ok(())
    }

    fn lower_switch(&mut self, block: BasicBlock<'ctx>, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        let switch = SwitchInst::new(instruction);
        let cond = self.materialize_value(switch.get_condition())?;
        let cases = switch.get_cases();
        let default_block = switch.get_default_block();
        let br_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::Br,
            &[("target", "default_label")],
        )?;

        if cases.is_empty() {
            self.lower_phi_moves(block, default_block)?;
            let env = LoweringEnv::new().label("default_label", self.label_for_block(default_block)?);
            self.emit_profile_action(&br_action, &env)?;
            return Ok(());
        }

        let default_edge = self.builder.new_label();
        let last_case = cases.len().saturating_sub(1);
        let icmp_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::Icmp,
            &[
                ("pred", "eq"),
                ("dst", "%vm"),
                ("lhs", "%vc"),
                ("rhs", "%vk"),
                ("width", "type_width(%cond)"),
            ],
        )?;
        let br_if_action = self.emit_action_for_shape(
            "llvm.switch.control",
            &HandlerSemantic::BrCond,
            &[
                ("cond", "%vm"),
                ("then_pc", "case_label"),
                ("else_pc", "next_case_label"),
            ],
        )?;
        for (index, (case_value, case_block)) in cases.into_iter().enumerate() {
            let case = self.materialize_value(case_value)?;
            let matched = self.alloc_temporary_vreg()?;
            let icmp_env = LoweringEnv::new()
                .binding("%vc", cond)
                .binding("%vk", case)
                .reg("%vm", matched, 1)
                .imm("eq", CmpPredicate::Eq as u64)
                .imm("type_width(%cond)", cond.width as u64);
            self.emit_profile_action(&icmp_action, &icmp_env)?;

            let case_edge = self.builder.new_label();
            let next_edge = if index == last_case {
                default_edge
            } else {
                self.builder.new_label()
            };
            let br_if_env = LoweringEnv::new()
                .reg("%vm", matched, 1)
                .label("case_label", case_edge)
                .label("next_case_label", next_edge);
            self.emit_profile_action(&br_if_action, &br_if_env)?;

            self.builder.bind_label(case_edge);
            self.lower_phi_moves(block, case_block)?;
            let case_env = LoweringEnv::new().label("default_label", self.label_for_block(case_block)?);
            self.emit_profile_action(&br_action, &case_env)?;

            if index != last_case {
                self.builder.bind_label(next_edge);
            }
        }

        self.builder.bind_label(default_edge);
        self.lower_phi_moves(block, default_block)?;
        let env = LoweringEnv::new().label("default_label", self.label_for_block(default_block)?);
        self.emit_profile_action(&br_action, &env)?;
        Ok(())
    }

    fn lower_return(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<()> {
        if instruction.get_num_operands() == 0 {
            let env = LoweringEnv::new().reg("x0", 0, 64);
            self.execute_lowering_rule("llvm.ret.void", env, Some(HandlerSemantic::Ret))?;
            return Ok(());
        }

        if !self.aggregate_return_fields.is_empty() {
            let mov_action = self.emit_action_for_shape(
                "llvm.ret.aggregate",
                &HandlerSemantic::Mov,
                &[("dst", "ret_slot"), ("src", "%vf"), ("width", "field_width(%field)")],
            )?;
            let ret_action =
                self.emit_action_for_shape("llvm.ret.aggregate", &HandlerSemantic::Ret, &[("src", "ret0")])?;
            let aggregate = self.aggregate_operand(instruction, 0)?;
            let return_fields = self.aggregate_return_fields.clone();
            for (index, field) in return_fields.iter().enumerate() {
                let ret_reg = self
                    .return_registers
                    .get(index)
                    .copied()
                    .with_context(|| format!("missing ABI return register {index}"))?;
                let src = aggregate
                    .fields
                    .get(index)
                    .copied()
                    .flatten()
                    .with_context(|| format!("aggregate return field {index} is out of range"))?;
                if src.width != field.width {
                    bail!(
                        "aggregate return field {index} width mismatch: value is {}, return type expects {}",
                        src.width,
                        field.width
                    );
                }
                if src.reg != ret_reg {
                    let env = LoweringEnv::new()
                        .reg("ret_slot", ret_reg, src.width)
                        .binding("%vf", src)
                        .imm("field_width(%field)", src.width as u64);
                    self.emit_profile_action(&mov_action, &env)?;
                }
            }
            let src = self
                .return_registers
                .first()
                .copied()
                .context("aggregate return has no ABI return register")?;
            let env = LoweringEnv::new().reg("ret0", src, 64);
            self.emit_profile_action(&ret_action, &env)?;
            return Ok(());
        }

        let ret = instruction_operand_value(instruction, 0)?;
        let env = LoweringEnv::new().llvm_source("%value", ret);
        self.execute_lowering_rule("llvm.ret.scalar", env, Some(HandlerSemantic::Ret))?;
        Ok(())
    }

    fn lower_phi_moves(&mut self, from: BasicBlock<'ctx>, to: BasicBlock<'ctx>) -> anyhow::Result<()> {
        for phi in leading_phi_nodes(to) {
            let dst = self
                .values
                .get(&instruction_key(phi))
                .copied()
                .context("missing destination binding for phi")?;
            let incoming = phi_incoming_value(phi, from)?;
            let src = self.materialize_value(incoming)?;
            let env = LoweringEnv::new()
                .binding("%incoming", src)
                .binding("%r", dst)
                .llvm_value("%r", instruction_key(phi))
                .binding("%vi", src)
                .binding("%vr", dst)
                .imm("type_width(%r)", dst.width as u64);
            self.execute_lowering_rule("llvm.phi.edge_move", env, Some(HandlerSemantic::Mov))?;
        }

        Ok(())
    }

    fn materialize_operand(&mut self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<ValueBinding> {
        let value = instruction
            .get_operand(index)
            .and_then(|operand| operand.value())
            .with_context(|| format!("missing value operand {index}"))?;
        self.materialize_value(value)
    }

    fn aggregate_seed_from_operand(
        &self,
        instruction: InstructionValue<'ctx>,
        index: u32,
    ) -> anyhow::Result<AggregateBinding> {
        let value = instruction
            .get_operand(index)
            .and_then(|operand| operand.value())
            .with_context(|| format!("missing aggregate operand {index}"))?;
        if let Some(binding) = self.aggregates.get(&value_key(value)) {
            return Ok(binding.clone());
        }

        Ok(AggregateBinding {
            fields: vec![None; aggregate_field_count(value.get_type())?],
        })
    }

    fn aggregate_operand(&self, instruction: InstructionValue<'ctx>, index: u32) -> anyhow::Result<AggregateBinding> {
        let value = instruction
            .get_operand(index)
            .and_then(|operand| operand.value())
            .with_context(|| format!("missing aggregate operand {index}"))?;
        self.aggregates
            .get(&value_key(value))
            .cloned()
            .context("aggregate value was not built by supported insertvalue lowering")
    }

    fn materialize_value(&mut self, value: BasicValueEnum<'ctx>) -> anyhow::Result<ValueBinding> {
        if let Some(binding) = self.values.get(&value_key(value)).copied() {
            return Ok(binding);
        }

        if value.is_int_value() {
            let int_value = value.into_int_value();
            if let Some(imm) = int_value.get_zero_extended_constant() {
                let width = checked_width(int_value.get_type().get_bit_width())?;
                let reg = self.alloc_temporary_vreg()?;
                self.push_constant(reg, imm, width)?;
                return Ok(ValueBinding { reg, width });
            }
        }

        bail!("only integer constants and previously lowered SSA values can be materialized")
    }

    fn push_constant(&mut self, dst: u8, imm: u64, width: u8) -> anyhow::Result<()> {
        if should_use_const_pool(imm, width) {
            let env = LoweringEnv::new()
                .imm("%v", imm)
                .reg("%vr", dst, width)
                .imm("const_pool_index(%v)", imm)
                .imm("type_width(%v)", width as u64);
            self.execute_lowering_rule("llvm.const_pool.materialize", env, Some(HandlerSemantic::ConstLoad))?;
        } else {
            self.emit_profile_mov_imm(dst, imm, width)?;
        }
        Ok(())
    }

    fn label_for_block(&self, block: BasicBlock<'ctx>) -> anyhow::Result<LabelId> {
        self.labels
            .get(&block_key(block))
            .copied()
            .context("missing label for successor")
    }

    fn static_alloca_count(&mut self, instruction: InstructionValue<'ctx>) -> anyhow::Result<u64> {
        let Some(operand) = instruction.get_operand(0).and_then(|operand| operand.value()) else {
            return Ok(1);
        };
        let count = operand
            .into_int_value()
            .get_zero_extended_constant()
            .context("dynamic alloca count is not supported")?;
        if count == 0 {
            bail!("zero-count alloca is not supported");
        }
        Ok(count)
    }

    fn gep_dynamic_terms(
        &mut self,
        instruction: InstructionValue<'ctx>,
        gep: GepInst<'ctx>,
    ) -> anyhow::Result<GepTerms> {
        // SAFETY: `instruction` 是正在 lowering 的 live LLVM getelementptr 指令。
        // C API 只读取 module 拥有的类型元数据；null 会立刻处理，因此 opaque 或畸形 GEP 会安全跳过。
        let source_type = unsafe { LLVMGetGEPSourceElementType(instruction.as_value_ref()) };
        if source_type.is_null() {
            bail!("getelementptr source element type is unavailable");
        }

        let mut current_type = source_type;
        let mut constant_offset = 0_i64;
        let mut dynamic = Vec::new();

        for (index_position, index) in gep.get_indices().into_iter().enumerate() {
            let index = index.context("getelementptr index has no value")?;
            let (scale, next_type) =
                gep_index_scale_and_next_type(&self.target_data, current_type, index_position, index)?;
            if let Some(constant) = index.into_int_value().get_sign_extended_constant() {
                let scaled = constant
                    .checked_mul(scale as i64)
                    .context("getelementptr constant offset overflow")?;
                constant_offset = constant_offset
                    .checked_add(scaled)
                    .context("getelementptr constant offset overflow")?;
            } else {
                dynamic.push((self.materialize_value(index)?, scale));
            }
            current_type = next_type;
        }

        Ok(GepTerms {
            constant_offset,
            dynamic,
        })
    }
}

fn native_call_target<'ctx>(function: FunctionValue<'ctx>) -> anyhow::Result<NativeCallTarget<'ctx>> {
    let fn_type = function.get_type();
    if fn_type.is_var_arg() {
        bail!("varargs native calls are not supported");
    }

    let (returns_void, return_fields) = match fn_type.get_return_type() {
        None => (true, Vec::new()),
        Some(BasicTypeEnum::IntType(return_type)) => (
            false,
            vec![ReturnField {
                width: checked_width(return_type.get_bit_width())?,
                is_pointer: false,
            }],
        ),
        Some(BasicTypeEnum::PointerType(_)) => (
            false,
            vec![ReturnField {
                width: 64,
                is_pointer: true,
            }],
        ),
        Some(BasicTypeEnum::StructType(return_type)) => {
            let fields = return_type
                .get_field_types()
                .into_iter()
                .enumerate()
                .map(|(index, ty)| return_field_from_type(ty).with_context(|| format!("native return field {index}")))
                .collect::<anyhow::Result<Vec<_>>>()?;
            if fields.is_empty() {
                bail!("empty aggregate native call returns are not supported");
            }
            (false, fields)
        },
        Some(_) => bail!("only void, scalar integer, pointer, and direct struct native call returns are supported"),
    };

    let param_types = fn_type.get_param_types();
    if param_types.len() > 8 {
        bail!("only up to 8 scalar integer native call arguments are supported");
    }
    let params = param_types
        .iter()
        .map(|ty| match ty {
            BasicMetadataTypeEnum::IntType(int_ty) => Ok((checked_width(int_ty.get_bit_width())?, false)),
            BasicMetadataTypeEnum::PointerType(_) => Ok((64, true)),
            _ => bail!("only scalar integer and pointer native call arguments are supported"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (param_widths, param_is_pointer) = params.into_iter().unzip();

    Ok(NativeCallTarget {
        function,
        param_widths,
        returns_void,
        return_fields,
        param_is_pointer,
    })
}

fn should_use_const_pool(imm: u64, width: u8) -> bool {
    width > 16 && imm > 0xff
}

fn x_register_list(name: &str, registers: &[VmRegister]) -> anyhow::Result<Vec<u8>> {
    registers
        .iter()
        .map(|register| match register {
            VmRegister::X(index) => Ok(*index),
            VmRegister::Q(index) => {
                bail!("{name} uses q{index}, but scalar native call lowering only supports x registers")
            },
        })
        .collect()
}

struct GepTerms {
    constant_offset: i64,
    dynamic: Vec<(ValueBinding, u64)>,
}

fn gep_index_scale_and_next_type(
    target_data: &TargetData,
    current_type: LLVMTypeRef,
    index_position: usize,
    index: BasicValueEnum<'_>,
) -> anyhow::Result<(u64, LLVMTypeRef)> {
    if !index.is_int_value() {
        bail!("getelementptr index is not an integer");
    }

    if index_position == 0 {
        return Ok((store_size(target_data, current_type)?, current_type));
    }

    // SAFETY: `current_type` 来自 LLVM 的 GEP source type，或来自 LLVM 先前返回的 element type。
    // match 只查询 type kind 元数据，不支持的形状会作为 safe-skip 错误返回。
    match unsafe { LLVMGetTypeKind(current_type) } {
        LLVMTypeKind::LLVMArrayTypeKind | LLVMTypeKind::LLVMVectorTypeKind => {
            // SAFETY: 上面的 type kind 保证对 array/vector 查询 element type 是有效的。
            // null 结果仍会在任何 size 计算前被拒绝。
            let element_type = unsafe { LLVMGetElementType(current_type) };
            if element_type.is_null() {
                bail!("getelementptr aggregate element type is unavailable");
            }
            Ok((store_size(target_data, element_type)?, element_type))
        },
        LLVMTypeKind::LLVMStructTypeKind => bail!("dynamic getelementptr through struct fields is not supported"),
        _ => Ok((store_size(target_data, current_type)?, current_type)),
    }
}

fn store_size(target_data: &TargetData, ty: LLVMTypeRef) -> anyhow::Result<u64> {
    // SAFETY: `target_data` 属于当前 module，`ty` 是从同一 context 取得的 LLVM type。
    // LLVM 在这里只读取 layout 元数据；零结果会被当作不支持处理，而不会继续用于 lowering。
    let size = unsafe { LLVMStoreSizeOfType(target_data.as_mut_ptr(), ty) };
    if size == 0 {
        bail!("zero-sized getelementptr element type is not supported");
    }
    Ok(size)
}

fn leading_phi_nodes(block: BasicBlock<'_>) -> Vec<InstructionValue<'_>> {
    block
        .get_instructions()
        .into_iter()
        .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        .collect()
}

fn phi_incoming_value<'ctx>(
    phi: InstructionValue<'ctx>,
    predecessor: BasicBlock<'ctx>,
) -> anyhow::Result<BasicValueEnum<'ctx>> {
    let phi_value = PhiInst::new(phi).into_phi_value();
    phi_value
        .get_incomings()
        .find_map(|(value, block)| (block == predecessor).then_some(value))
        .context("phi has no incoming value for predecessor")
}

fn instruction_result_width(instruction: InstructionValue<'_>) -> anyhow::Result<Option<u8>> {
    match instruction.get_opcode() {
        InstructionOpcode::Br
        | InstructionOpcode::Return
        | InstructionOpcode::Store
        | InstructionOpcode::InsertValue => {
            return Ok(None);
        },
        _ => {},
    }

    match instruction.get_type() {
        AnyTypeEnum::IntType(int_ty) => checked_width(int_ty.get_bit_width()).map(Some),
        AnyTypeEnum::PointerType(_) => Ok(Some(64)),
        AnyTypeEnum::VoidType(_) => Ok(None),
        AnyTypeEnum::StructType(_) if instruction.get_opcode() == InstructionOpcode::Call => Ok(None),
        other => bail!("unsupported instruction result type: {other:?}"),
    }
}

fn return_field_from_type(ty: BasicTypeEnum<'_>) -> anyhow::Result<ReturnField> {
    match ty {
        BasicTypeEnum::IntType(int_ty) => Ok(ReturnField {
            width: checked_width(int_ty.get_bit_width())?,
            is_pointer: false,
        }),
        BasicTypeEnum::PointerType(_) => Ok(ReturnField {
            width: 64,
            is_pointer: true,
        }),
        other => bail!("unsupported aggregate return field type: {other:?}"),
    }
}

fn aggregate_field_count(ty: BasicTypeEnum<'_>) -> anyhow::Result<usize> {
    match ty {
        BasicTypeEnum::StructType(ty) => Ok(ty.count_fields() as usize),
        BasicTypeEnum::ArrayType(ty) => Ok(ty.len() as usize),
        other => bail!("unsupported aggregate value type: {other:?}"),
    }
}

fn aggregate_single_index(instruction: InstructionValue<'_>) -> anyhow::Result<usize> {
    let indices = instruction.get_indices();
    if indices.len() != 1 {
        bail!("only single-level aggregate indices are supported");
    }
    Ok(indices[0] as usize)
}

fn instruction_value_operands(instruction: InstructionValue<'_>) -> Vec<BasicValueEnum<'_>> {
    let operand_count = match instruction.get_opcode() {
        InstructionOpcode::Call => instruction.get_num_operands().saturating_sub(1),
        _ => instruction.get_num_operands(),
    };

    (0..operand_count)
        .filter_map(|index| instruction.get_operand(index).and_then(|operand| operand.value()))
        .collect()
}

fn instruction_operand_value(instruction: InstructionValue<'_>, index: u32) -> anyhow::Result<BasicValueEnum<'_>> {
    instruction
        .get_operand(index)
        .and_then(|operand| operand.value())
        .with_context(|| format!("missing value operand {index}"))
}

fn value_width(value: BasicValueEnum<'_>) -> anyhow::Result<u8> {
    match value.get_type() {
        BasicTypeEnum::IntType(int_ty) => checked_width(int_ty.get_bit_width()),
        BasicTypeEnum::PointerType(_) => Ok(64),
        other => bail!("unsupported scalar value type: {other:?}"),
    }
}

fn checked_width(width: u32) -> anyhow::Result<u8> {
    if matches!(width, 1 | 8 | 16 | 32 | 64) {
        Ok(width as u8)
    } else {
        bail!("unsupported integer width: {width}")
    }
}

fn checked_width_u64(width: u64) -> anyhow::Result<u8> {
    u32::try_from(width)
        .context("integer width does not fit in u32")
        .and_then(checked_width)
}

fn width_from_operand_type(value_type: &str) -> u8 {
    value_type
        .strip_prefix('i')
        .and_then(|width| width.parse::<u8>().ok())
        .filter(|width| matches!(width, 1 | 8 | 16 | 32 | 64))
        .unwrap_or(64)
}

fn width_from_lowering_value_type(value_type: &str, env: &LoweringEnv<'_>) -> anyhow::Result<u8> {
    if let Some(width) = value_type.strip_prefix('i').and_then(|width| width.parse::<u64>().ok()) {
        return checked_width_u64(width);
    }

    if matches!(value_type, "integer" | "call_result" | "aggregate") {
        for expression in ["type_width(%r)", "memory_width(%ptr)", "type_width(%field)"] {
            if let Ok(LoweringValue::Imm(width)) = env.get(expression) {
                return checked_width_u64(width);
            }
        }
    }

    Ok(width_from_operand_type(value_type))
}

fn parse_u64_literal(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .map_or_else(|| value.parse::<u64>().ok(), |hex| u64::from_str_radix(hex, 16).ok())
}

fn predicate_from_u64(value: u64) -> anyhow::Result<CmpPredicate> {
    match value {
        value if value == CmpPredicate::Eq as u64 => Ok(CmpPredicate::Eq),
        value if value == CmpPredicate::Ne as u64 => Ok(CmpPredicate::Ne),
        value if value == CmpPredicate::Ugt as u64 => Ok(CmpPredicate::Ugt),
        value if value == CmpPredicate::Uge as u64 => Ok(CmpPredicate::Uge),
        value if value == CmpPredicate::Ult as u64 => Ok(CmpPredicate::Ult),
        value if value == CmpPredicate::Ule as u64 => Ok(CmpPredicate::Ule),
        value if value == CmpPredicate::Sgt as u64 => Ok(CmpPredicate::Sgt),
        value if value == CmpPredicate::Sge as u64 => Ok(CmpPredicate::Sge),
        value if value == CmpPredicate::Slt as u64 => Ok(CmpPredicate::Slt),
        value if value == CmpPredicate::Sle as u64 => Ok(CmpPredicate::Sle),
        other => bail!("unsupported comparison predicate value {other}"),
    }
}

fn map_predicate(predicate: IntPredicate) -> CmpPredicate {
    match predicate {
        IntPredicate::EQ => CmpPredicate::Eq,
        IntPredicate::NE => CmpPredicate::Ne,
        IntPredicate::UGT => CmpPredicate::Ugt,
        IntPredicate::UGE => CmpPredicate::Uge,
        IntPredicate::ULT => CmpPredicate::Ult,
        IntPredicate::ULE => CmpPredicate::Ule,
        IntPredicate::SGT => CmpPredicate::Sgt,
        IntPredicate::SGE => CmpPredicate::Sge,
        IntPredicate::SLT => CmpPredicate::Slt,
        IntPredicate::SLE => CmpPredicate::Sle,
    }
}

fn value_key(value: BasicValueEnum<'_>) -> ValueKey {
    value.as_value_ref() as usize
}

fn instruction_key(instruction: InstructionValue<'_>) -> ValueKey {
    instruction.as_value_ref() as usize
}

fn block_key(block: BasicBlock<'_>) -> BlockKey {
    block.as_mut_ptr() as usize
}

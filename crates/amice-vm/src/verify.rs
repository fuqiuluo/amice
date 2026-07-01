//! AMICE VMP profile 契约的跨文件 verifier。
//!
//! # 契约
//! parser 只接受已知 DSL 语句，而 verifier 负责确认解析后的 package 足够一致，
//! 可以驱动 lowering、encoding 和 runtime emission。pass 代码必须在 verifier 出错时拒绝
//! profile，避免用非法契约生成半虚拟化函数。
//!
//! # 不变量
//! - scope 取值仅限 `func` 和 `module`。
//! - 寄存器组必须严格为 `x0..x31` 和 `q0..q64`。
//! - 当 `q.lowering = disabled` 时，ABI、lowering 或 ISA semantic 中的 q 依赖都是硬错误。
//! - lowering action 必须在 `emit`/`bind` 消费值前先定义这些值。

use crate::abi::{NativeCallPolicy, VmRegister};
use crate::isa::{
    AtomicRmwOp, BinOp, CastOp, FloatBinOp, FloatCastOp, FloatUnaryOp, HandlerSemantic, InstructionDesc, IntTernaryOp,
    IntUnaryOp, OperandDesc, OperandKind, SUPPORTED_DECODED_WIDTHS, SuperOp,
};
use crate::lowering::{NATIVE_CALL_MAX_ARGS, NATIVE_CALL_MAX_RETURNS};
use crate::profile::{
    ConstPoolEncryption, DecoderStep, LoweringAction, OpcodeEncoding, OperandEncoding, ProfileError, ProfilePackage,
    REQUIRED_LOWERING_MATCHES, RelocBase, RelocWidth, RuntimeScope, SegmentMode,
};
use crate::runtime::{DispatchStrategy, HandlerClonePolicy, WideRegisterPolicy};
use std::collections::HashSet;

/// 校验 VMP runtime 所需的 profile package 不变量。
///
/// # 错误
/// 当 target 约束、寄存器组、别名、ABI 映射、ISA semantic、lowering action、
/// bytecode layout、decoder step 或 runtime enhancement 违反当前支持的 VMP 契约时，
/// 返回 `ProfileError::Invalid`。
///
/// # 契约
/// 此函数必须在解析后、LLVM pass 翻译任何函数前运行。成功返回表示 package 对当前标量
/// 整数/指针实现边界而言结构上安全。
pub fn verify_profile(profile: &ProfilePackage) -> Result<(), ProfileError> {
    if profile.manifest.target.pointer_bits != 64 {
        return Err(ProfileError::Invalid(format!(
            "target.pointer_bits must be 64, got {}",
            profile.manifest.target.pointer_bits
        )));
    }

    if profile.manifest.target.endian != "little" {
        return Err(ProfileError::Invalid(format!(
            "target.endian must be little, got {}",
            profile.manifest.target.endian
        )));
    }

    match profile.runtime.scope {
        RuntimeScope::Func | RuntimeScope::Module => {},
    }
    match profile.bytecode.scope {
        RuntimeScope::Func | RuntimeScope::Module => {},
    }
    match profile.runtime.polymorph_scope {
        RuntimeScope::Func | RuntimeScope::Module => {},
    }

    match profile.runtime.dispatch {
        DispatchStrategy::Switch => {},
    }

    for alias in ["lr", "sp"] {
        if !profile.runtime.aliases.contains_key(alias) {
            return Err(ProfileError::Invalid(format!("runtime.vm must define alias {alias}")));
        }
    }

    verify_register_banks(profile)?;
    verify_wide_register_policy(profile)?;
    verify_control_state(profile)?;
    verify_runtime_enhancements(profile)?;
    verify_abi(profile)?;

    for (alias, register) in &profile.runtime.aliases {
        if !is_valid_register(register) {
            return Err(ProfileError::Invalid(format!(
                "runtime alias {alias} points to invalid register {register}"
            )));
        }
    }

    if !profile.isa.has_unique_opcodes() {
        return Err(ProfileError::Invalid("ISA opcodes must be unique".to_owned()));
    }

    let mut names = HashSet::with_capacity(profile.isa.instructions.len());
    for instruction in &profile.isa.instructions {
        if instruction.opcodes().is_empty() {
            return Err(ProfileError::Invalid(format!(
                "ISA instruction {} must declare at least one opcode alias",
                instruction.name
            )));
        }
        if !names.insert(instruction.name.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "duplicate ISA instruction name {}",
                instruction.name
            )));
        }
    }

    verify_instruction_effects(profile)?;
    verify_instruction_operands(profile)?;
    verify_instruction_record_widths(profile)?;
    verify_required_isa(profile)?;
    verify_lowering_rules(profile)?;
    verify_bytecode(profile)?;
    verify_decoder_steps(&profile.decoder.steps)?;

    Ok(())
}

fn verify_instruction_operands(profile: &ProfilePackage) -> Result<(), ProfileError> {
    for instruction in &profile.isa.instructions {
        if instruction.operand_descs.len() != instruction.operands as usize {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} operand count disagrees with parsed descriptors",
                instruction.name
            )));
        }

        let expected = expected_operands(&instruction.semantic);
        if instruction.operand_descs.len() != expected.len() {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} must declare {} operands, got {}",
                instruction.name,
                expected.len(),
                instruction.operand_descs.len()
            )));
        }

        for (expected_name, expected_kind) in expected {
            let actual = instruction
                .operand_descs
                .iter()
                .find(|operand| operand.name == expected_name)
                .ok_or_else(|| {
                    ProfileError::Invalid(format!(
                        "isa.vm instruction {} must declare operand {}:{:?}",
                        instruction.name, expected_name, expected_kind
                    ))
                })?;
            if actual.kind != expected_kind {
                return Err(ProfileError::Invalid(format!(
                    "isa.vm instruction {} operand {} must be {:?}, got {:?}",
                    instruction.name, expected_name, expected_kind, actual.kind
                )));
            }
        }
    }

    Ok(())
}

fn verify_instruction_record_widths(profile: &ProfilePackage) -> Result<(), ProfileError> {
    let allowed = &profile.bytecode.instruction_record.decoded_widths;
    if allowed.is_empty() {
        return Err(ProfileError::Invalid(
            "bytecode.vm decoded_width one_of list must not be empty".to_owned(),
        ));
    }
    for width in allowed {
        if !SUPPORTED_DECODED_WIDTHS.contains(width) {
            return Err(ProfileError::Invalid(format!(
                "bytecode.vm decoded_width {width} is not supported; supported widths are {:?}",
                SUPPORTED_DECODED_WIDTHS
            )));
        }
    }
    if !allowed.contains(&profile.bytecode.instruction_record.default_decoded_width) {
        return Err(ProfileError::Invalid(format!(
            "bytecode.vm decoded_width default {} is not in {:?}",
            profile.bytecode.instruction_record.default_decoded_width, allowed
        )));
    }

    for instruction in &profile.isa.instructions {
        if !allowed.contains(&instruction.decoded_width) {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} decoded_width {} is not allowed by bytecode.vm {:?}",
                instruction.name, instruction.decoded_width, allowed
            )));
        }
        let needed = worst_case_record_len(instruction)?;
        if needed > instruction.decoded_width as usize {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} decoded_width {} cannot hold opcode/operands; worst case needs {needed} bytes",
                instruction.name, instruction.decoded_width
            )));
        }
    }

    Ok(())
}

fn worst_case_record_len(instruction: &InstructionDesc) -> Result<usize, ProfileError> {
    let max_opcode = instruction
        .opcodes()
        .iter()
        .copied()
        .max()
        .ok_or_else(|| ProfileError::Invalid(format!("ISA instruction {} has no opcode", instruction.name)))?;
    let opcode_len = varint_len(max_opcode as u64);
    let operand_len = instruction
        .operand_descs
        .iter()
        .map(worst_case_bitpacked_operand_len)
        .sum::<Result<usize, _>>()?;
    Ok(opcode_len + operand_len)
}

fn worst_case_bitpacked_operand_len(operand: &OperandDesc) -> Result<usize, ProfileError> {
    let max_bits = match operand.kind {
        OperandKind::VReg => 5,
        OperandKind::Imm => immediate_type_bits(&operand.value_type)?,
        OperandKind::ConstPoolIndex | OperandKind::Label => 64,
        OperandKind::Unknown => 64,
    };
    Ok(1 + max_bits.div_ceil(7))
}

fn immediate_type_bits(value_type: &str) -> Result<usize, ProfileError> {
    match value_type {
        "i1" | "u1" => Ok(1),
        "i8" | "u8" => Ok(7),
        "i16" | "u16" => Ok(16),
        "i32" | "u32" => Ok(32),
        "i64" | "u64" | "ptr" | "usize" | "label" | "const_pool_index" => Ok(64),
        other => Err(ProfileError::Invalid(format!(
            "unsupported immediate operand value type {other} for decoded_width capacity check"
        ))),
    }
}

fn varint_len(mut value: u64) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

fn expected_operands(semantic: &HandlerSemantic) -> Vec<(String, OperandKind)> {
    use HandlerSemantic::*;
    use OperandKind::*;

    match semantic {
        MovImm => operands([("dst", VReg), ("imm", Imm), ("width", Imm)]),
        ConstLoad => operands([("dst", VReg), ("index", ConstPoolIndex), ("width", Imm)]),
        Super(crate::isa::SuperOp::AddXor) => operands([
            ("dst", VReg),
            ("lhs", VReg),
            ("rhs", VReg),
            ("xor_rhs", VReg),
            ("width", Imm),
        ]),
        Super(crate::isa::SuperOp::IcmpBrIf) => operands([
            ("pred", Imm),
            ("lhs", VReg),
            ("rhs", VReg),
            ("width", Imm),
            ("then_pc", Label),
            ("else_pc", Label),
        ]),
        Super(crate::isa::SuperOp::GepLoad) => {
            operands([("dst", VReg), ("base", VReg), ("offset", Imm), ("width", Imm)])
        },
        Super(crate::isa::SuperOp::LoadAdd) => {
            operands([("dst", VReg), ("ptr", VReg), ("addend", VReg), ("width", Imm)])
        },
        Mov => operands([("dst", VReg), ("src", VReg), ("width", Imm)]),
        Bin(_) => operands([("dst", VReg), ("lhs", VReg), ("rhs", VReg), ("width", Imm)]),
        IntUnary(_) => operands([("dst", VReg), ("src", VReg), ("width", Imm)]),
        IntTernary(_) => operands([
            ("dst", VReg),
            ("lhs", VReg),
            ("rhs", VReg),
            ("third", VReg),
            ("width", Imm),
        ]),
        FloatBin(_) => operands([("dst", VReg), ("lhs", VReg), ("rhs", VReg), ("width", Imm)]),
        FloatUnary(_) => operands([("dst", VReg), ("src", VReg), ("width", Imm)]),
        FloatCast(_) => operands([("dst", VReg), ("src", VReg), ("from_width", Imm), ("to_width", Imm)]),
        Icmp => operands([
            ("pred", Imm),
            ("dst", VReg),
            ("lhs", VReg),
            ("rhs", VReg),
            ("width", Imm),
        ]),
        Fcmp => operands([
            ("pred", Imm),
            ("dst", VReg),
            ("lhs", VReg),
            ("rhs", VReg),
            ("width", Imm),
        ]),
        Cast(_) => operands([("dst", VReg), ("src", VReg), ("from_width", Imm), ("to_width", Imm)]),
        Alloca => operands([("dst", VReg), ("bytes", Imm), ("align", Imm)]),
        Load => operands([("dst", VReg), ("ptr", VReg), ("width", Imm)]),
        Store => operands([("src", VReg), ("ptr", VReg), ("width", Imm)]),
        AtomicLoad => operands([("dst", VReg), ("ptr", VReg), ("width", Imm), ("ordering", Imm)]),
        AtomicStore => operands([("src", VReg), ("ptr", VReg), ("width", Imm), ("ordering", Imm)]),
        AtomicRmw(_) => operands([
            ("dst", VReg),
            ("ptr", VReg),
            ("src", VReg),
            ("width", Imm),
            ("ordering", Imm),
        ]),
        CmpXchg => operands([
            ("old", VReg),
            ("success", VReg),
            ("ptr", VReg),
            ("cmp", VReg),
            ("new", VReg),
            ("width", Imm),
            ("success_ordering", Imm),
            ("failure_ordering", Imm),
        ]),
        Fence => operands([("ordering", Imm)]),
        Gep => operands([("dst", VReg), ("base", VReg), ("offset", Imm)]),
        CallNative => call_native_operand_contract(),
        Nop => Vec::new(),
        Br => operands([("target", Label)]),
        BrCond => operands([("cond", VReg), ("then_pc", Label), ("else_pc", Label)]),
        VmCall => operands([("target", Label)]),
        VmRet => Vec::new(),
        Ret => operands([("src", VReg)]),
    }
}

fn operands<const N: usize>(items: [(&str, OperandKind); N]) -> Vec<(String, OperandKind)> {
    items.into_iter().map(|(name, kind)| (name.to_owned(), kind)).collect()
}

fn call_native_operand_contract() -> Vec<(String, OperandKind)> {
    use OperandKind::*;

    let mut operands = operands([
        ("callee", Imm),
        ("argc", Imm),
        ("arg0", VReg),
        ("arg1", VReg),
        ("arg2", VReg),
        ("arg3", VReg),
        ("arg4", VReg),
        ("arg5", VReg),
        ("arg6", VReg),
        ("arg7", VReg),
        ("ret_count", Imm),
    ]);
    operands.extend(
        (0..NATIVE_CALL_MAX_RETURNS)
            .flat_map(|index| [(format!("ret{index}"), VReg), (format!("ret{index}_width"), Imm)]),
    );
    operands
}

fn verify_instruction_effects(profile: &ProfilePackage) -> Result<(), ProfileError> {
    for instruction in &profile.isa.instructions {
        let expected = instruction.semantic.expected_effect();
        if instruction.effect.pc != expected.pc {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} has pc effect {:?}, expected {:?}",
                instruction.name, instruction.effect.pc, expected.pc
            )));
        }
        if instruction.effect.memory_read != expected.memory_read {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} memory_read effect is {}, expected {}",
                instruction.name, instruction.effect.memory_read, expected.memory_read
            )));
        }
        if instruction.effect.memory_write != expected.memory_write {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} memory_write effect is {}, expected {}",
                instruction.name, instruction.effect.memory_write, expected.memory_write
            )));
        }
        if instruction.effect.native_call != expected.native_call {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {} native_call effect is {}, expected {}",
                instruction.name, instruction.effect.native_call, expected.native_call
            )));
        }
        verify_register_effect_subset(
            &instruction.name,
            "reads",
            &expected.register_reads,
            &instruction.effect.register_reads,
        )?;
        verify_register_effect_subset(
            &instruction.name,
            "writes",
            &expected.register_writes,
            &instruction.effect.register_writes,
        )?;
    }

    Ok(())
}

fn verify_register_effect_subset(
    instruction: &str,
    kind: &str,
    expected: &[String],
    actual: &[String],
) -> Result<(), ProfileError> {
    for register in expected {
        if !actual.iter().any(|actual| actual == register) {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {instruction} effect {kind} missing {register}"
            )));
        }
    }
    for register in actual {
        if !expected.iter().any(|expected| expected == register) {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {instruction} effect {kind} contains unexpected {register}"
            )));
        }
    }

    Ok(())
}

fn verify_wide_register_policy(profile: &ProfilePackage) -> Result<(), ProfileError> {
    match profile.runtime.q_lowering {
        WideRegisterPolicy::Disabled => {
            // 禁用 q path 是显式能力边界，不是提示。这里拒绝所有可能需要物化 q 寄存器的
            // profile 表面，避免生成的 runtime 假装支持 v128 lowering，却把值悄悄塞进 x 寄存器。
            for (alias, register) in &profile.runtime.aliases {
                if is_q_register(register) {
                    return Err(ProfileError::Invalid(format!(
                        "runtime alias {alias} points to q register {register} while q.lowering is disabled"
                    )));
                }
            }
            reject_q_indices("abi.vm host_to_vm vector arguments", &profile.abi.vector_args)?;
            reject_q_indices("abi.vm host_to_vm vector returns", &profile.abi.vector_returns)?;
            reject_q_registers("abi.vm vm_call call_args", &profile.abi.vm_call_args)?;
            reject_q_registers("abi.vm vm_call ret_values", &profile.abi.vm_call_returns)?;
            reject_q_registers("abi.vm native_call args", &profile.abi.native_args)?;
            reject_q_registers("abi.vm native_call returns", &profile.abi.native_returns)?;
            reject_q_registers("abi.vm native_call clobbers", &profile.abi.native_clobbers)?;
            for instruction in &profile.isa.instructions {
                if instruction
                    .operand_descs
                    .iter()
                    .any(|operand| operand.value_type == "v128")
                {
                    return Err(ProfileError::Invalid(format!(
                        "isa.vm instruction {} uses v128 operands while q.lowering is disabled",
                        instruction.name
                    )));
                }
                if let Some(register) = instruction.semantic_program.q_register_references.first() {
                    return Err(ProfileError::Invalid(format!(
                        "isa.vm instruction {} references {register} while q.lowering is disabled",
                        instruction.name
                    )));
                }
            }
            if let Some(register) = profile.lowering.q_register_references.first() {
                return Err(ProfileError::Invalid(format!(
                    "lowering.vm references {register} while q.lowering is disabled"
                )));
            }
            Ok(())
        },
    }
}

fn verify_register_banks(profile: &ProfilePackage) -> Result<(), ProfileError> {
    let x = profile
        .runtime
        .banks
        .iter()
        .find(|bank| bank.name == "x")
        .ok_or_else(|| ProfileError::Invalid("runtime.vm must declare bank x".to_owned()))?;
    if x.first != 0 || x.last != 31 || x.value_type != "u64" {
        return Err(ProfileError::Invalid(
            "runtime.vm bank x must be range x0..x31 type u64".to_owned(),
        ));
    }

    let q = profile
        .runtime
        .banks
        .iter()
        .find(|bank| bank.name == "q")
        .ok_or_else(|| ProfileError::Invalid("runtime.vm must declare bank q".to_owned()))?;
    if q.first != 0 || q.last != 64 || q.value_type != "v128" {
        return Err(ProfileError::Invalid(
            "runtime.vm bank q must be range q0..q64 type v128".to_owned(),
        ));
    }

    Ok(())
}

fn verify_control_state(profile: &ProfilePackage) -> Result<(), ProfileError> {
    let has_pc = profile
        .runtime
        .control_state
        .iter()
        .any(|slot| slot.name == "pc" && slot.value_type == "label");
    if !has_pc {
        return Err(ProfileError::Invalid(
            "runtime.vm must declare control_state pc: label".to_owned(),
        ));
    }

    Ok(())
}

fn verify_abi(profile: &ProfilePackage) -> Result<(), ProfileError> {
    if !profile.abi.call_link_declared {
        return Err(ProfileError::Invalid(
            "abi.vm vm_call must declare call_link".to_owned(),
        ));
    }
    if !profile.abi.ret_pc_declared {
        return Err(ProfileError::Invalid("abi.vm vm_call must declare ret_pc".to_owned()));
    }
    if profile.abi.lr_alias != "lr" {
        return Err(ProfileError::Invalid("abi.vm vm_call call_link must use lr".to_owned()));
    }
    if profile.abi.ret_pc_alias != "lr" {
        return Err(ProfileError::Invalid("abi.vm vm_call ret_pc must use lr".to_owned()));
    }
    for alias in [&profile.abi.lr_alias, &profile.abi.ret_pc_alias] {
        if !profile.runtime.aliases.contains_key(alias) {
            return Err(ProfileError::Invalid(format!(
                "abi.vm references alias {alias} but runtime.vm does not define it"
            )));
        }
    }
    if profile.abi.max_returns == 0 {
        return Err(ProfileError::Invalid(
            "abi.vm max_returns must be greater than zero".to_owned(),
        ));
    }
    if profile.abi.integer_returns.is_empty() {
        return Err(ProfileError::Invalid(
            "abi.vm must declare at least ret0 mapping".to_owned(),
        ));
    }
    if profile.abi.integer_returns.len() > profile.abi.max_returns as usize {
        return Err(ProfileError::Invalid(format!(
            "abi.vm declares {} integer returns but max_returns is {}",
            profile.abi.integer_returns.len(),
            profile.abi.max_returns
        )));
    }
    for (index, register) in profile.abi.integer_returns.iter().enumerate() {
        if *register >= 32 {
            return Err(ProfileError::Invalid(format!(
                "abi.vm integer return ret{index} register x{register} is out of range"
            )));
        }
    }
    verify_unique_integer_registers("abi.vm integer returns", &profile.abi.integer_returns)?;
    if profile.abi.integer_return != profile.abi.integer_returns[0] {
        return Err(ProfileError::Invalid(format!(
            "abi.vm ret0 mapping x{} does not match primary return x{}",
            profile.abi.integer_returns[0], profile.abi.integer_return
        )));
    }
    for register in &profile.abi.integer_args {
        if *register >= 32 {
            return Err(ProfileError::Invalid(format!(
                "abi.vm integer argument register x{register} is out of range"
            )));
        }
    }
    verify_unique_integer_registers("abi.vm integer arguments", &profile.abi.integer_args)?;
    for (index, register) in profile.abi.vector_args.iter().enumerate() {
        if *register >= 65 {
            return Err(ProfileError::Invalid(format!(
                "abi.vm vector argument vec{index} register q{register} is out of range"
            )));
        }
    }
    for (index, register) in profile.abi.vector_returns.iter().enumerate() {
        if *register >= 65 {
            return Err(ProfileError::Invalid(format!(
                "abi.vm vector return vret{index} register q{register} is out of range"
            )));
        }
    }
    if profile.abi.integer_args.len() > 8 {
        return Err(ProfileError::Invalid(format!(
            "abi.vm maps {} integer arguments but the current native wrapper supports at most 8",
            profile.abi.integer_args.len()
        )));
    }
    verify_abi_register_list("abi.vm vm_call call_args", &profile.abi.vm_call_args)?;
    verify_abi_register_list("abi.vm vm_call ret_values", &profile.abi.vm_call_returns)?;
    verify_abi_register_list("abi.vm native_call args", &profile.abi.native_args)?;
    verify_abi_register_list("abi.vm native_call returns", &profile.abi.native_returns)?;
    verify_abi_register_list("abi.vm native_call clobbers", &profile.abi.native_clobbers)?;
    match profile.abi.native_policy {
        NativeCallPolicy::Direct => {},
    }
    if profile.abi.vm_call_returns.len() > profile.abi.max_returns as usize {
        return Err(ProfileError::Invalid(format!(
            "abi.vm vm_call declares {} return registers but max_returns is {}",
            profile.abi.vm_call_returns.len(),
            profile.abi.max_returns
        )));
    }
    if profile.abi.native_args.len() > NATIVE_CALL_MAX_ARGS {
        return Err(ProfileError::Invalid(format!(
            "abi.vm native_call maps {} arguments but call_native currently supports at most {}",
            profile.abi.native_args.len(),
            NATIVE_CALL_MAX_ARGS
        )));
    }
    if profile.abi.native_returns.len() > profile.abi.max_returns as usize {
        return Err(ProfileError::Invalid(format!(
            "abi.vm native_call declares {} return registers but max_returns is {}",
            profile.abi.native_returns.len(),
            profile.abi.max_returns
        )));
    }
    if profile.abi.native_returns.len() > NATIVE_CALL_MAX_RETURNS {
        return Err(ProfileError::Invalid(format!(
            "abi.vm native_call maps {} returns but call_native currently supports at most {}",
            profile.abi.native_returns.len(),
            NATIVE_CALL_MAX_RETURNS
        )));
    }
    Ok(())
}

fn verify_runtime_enhancements(profile: &ProfilePackage) -> Result<(), ProfileError> {
    match profile.runtime.enhancements.handler_clone {
        HandlerClonePolicy::Disabled | HandlerClonePolicy::PerFunction => {},
    }

    for (name, enabled) in [
        ("threaded_dispatch", profile.runtime.enhancements.threaded_dispatch),
        (
            "indirect_branch_dispatch",
            profile.runtime.enhancements.indirect_branch_dispatch,
        ),
    ] {
        if enabled {
            return Err(ProfileError::Invalid(format!(
                "runtime.vm enhancement {name} is declared but the current LLVM emitter only implements switch dispatch"
            )));
        }
    }

    if !profile.runtime.enhancements.opcode_alias
        && profile
            .isa
            .instructions
            .iter()
            .any(|instruction| instruction.opcodes().len() > 1)
    {
        return Err(ProfileError::Invalid(
            "runtime.vm must enable opcode_alias when isa.vm declares opcode aliases".to_owned(),
        ));
    }

    Ok(())
}

fn verify_unique_integer_registers(name: &str, registers: &[u8]) -> Result<(), ProfileError> {
    let mut seen = HashSet::new();
    for register in registers {
        if !seen.insert(*register) {
            return Err(ProfileError::Invalid(format!("{name} maps x{register} more than once")));
        }
    }

    Ok(())
}

fn verify_abi_register_list(name: &str, registers: &[VmRegister]) -> Result<(), ProfileError> {
    for register in registers {
        match register {
            VmRegister::X(index) if *index < 32 => {},
            VmRegister::Q(index) if *index < 65 => {},
            VmRegister::X(index) => {
                return Err(ProfileError::Invalid(format!(
                    "{name} contains out-of-range x register x{index}"
                )));
            },
            VmRegister::Q(index) => {
                return Err(ProfileError::Invalid(format!(
                    "{name} contains out-of-range q register q{index}"
                )));
            },
        }
    }

    Ok(())
}

fn reject_q_registers(name: &str, registers: &[VmRegister]) -> Result<(), ProfileError> {
    for register in registers {
        if let VmRegister::Q(index) = register {
            return Err(ProfileError::Invalid(format!(
                "{name} references q{index} while q.lowering is disabled"
            )));
        }
    }

    Ok(())
}

fn reject_q_indices(name: &str, registers: &[u8]) -> Result<(), ProfileError> {
    for register in registers {
        return Err(ProfileError::Invalid(format!(
            "{name} references q{register} while q.lowering is disabled"
        )));
    }

    Ok(())
}

fn verify_required_isa(profile: &ProfilePackage) -> Result<(), ProfileError> {
    use BinOp::*;
    use CastOp::*;
    use FloatBinOp::{Add as FAdd, Div as FDiv, Mul as FMul, Rem as FRem, Sub as FSub};
    use FloatCastOp::{
        FloatExt as FFPExt, FloatToSignedInt as FFPToSI, FloatToUnsignedInt as FFPToUI, FloatTrunc as FFPTrunc,
        SignedIntToFloat as FSIToFP, UnsignedIntToFloat as FUIToFP,
    };
    use FloatUnaryOp::Neg as FNeg;
    use HandlerSemantic::*;
    use IntTernaryOp::{FShl, FShr};
    use IntUnaryOp::{BSwap, BitReverse, CtPop};

    let required = [
        (MovImm, 3, "mov_imm"),
        (ConstLoad, 3, "const_load"),
        (Mov, 3, "mov"),
        (Bin(Add), 4, "iadd"),
        (Bin(Sub), 4, "isub"),
        (Bin(Mul), 4, "imul"),
        (Bin(UDiv), 4, "iudiv"),
        (Bin(SDiv), 4, "isdiv"),
        (Bin(URem), 4, "iurem"),
        (Bin(SRem), 4, "isrem"),
        (Bin(Xor), 4, "ixor"),
        (Bin(And), 4, "iand"),
        (Bin(Or), 4, "ior"),
        (Bin(Shl), 4, "ishl"),
        (Bin(LShr), 4, "ilshr"),
        (Bin(AShr), 4, "iashr"),
        (IntUnary(CtPop), 3, "ctpop"),
        (IntUnary(BSwap), 3, "bswap"),
        (IntUnary(BitReverse), 3, "bitreverse"),
        (IntTernary(FShl), 5, "fshl"),
        (IntTernary(FShr), 5, "fshr"),
        (FloatBin(FAdd), 4, "fadd"),
        (FloatBin(FSub), 4, "fsub"),
        (FloatBin(FMul), 4, "fmul"),
        (FloatBin(FDiv), 4, "fdiv"),
        (FloatBin(FRem), 4, "frem"),
        (FloatUnary(FNeg), 3, "fneg"),
        (FloatCast(FSIToFP), 4, "sitofp"),
        (FloatCast(FUIToFP), 4, "uitofp"),
        (FloatCast(FFPToSI), 4, "fptosi"),
        (FloatCast(FFPToUI), 4, "fptoui"),
        (FloatCast(FFPTrunc), 4, "fptrunc"),
        (FloatCast(FFPExt), 4, "fpext"),
        (Icmp, 5, "icmp"),
        (Fcmp, 5, "fcmp"),
        (Cast(ZExt), 4, "zext"),
        (Cast(SExt), 4, "sext"),
        (Cast(Trunc), 4, "trunc"),
        (Cast(Bitcast), 4, "bitcast"),
        (Alloca, 3, "alloca"),
        (Load, 3, "load"),
        (Store, 3, "store"),
        (AtomicLoad, 4, "atomic_load"),
        (AtomicStore, 4, "atomic_store"),
        (AtomicRmw(AtomicRmwOp::Xchg), 5, "atomic_rmw_xchg"),
        (AtomicRmw(AtomicRmwOp::Add), 5, "atomic_rmw_add"),
        (AtomicRmw(AtomicRmwOp::Sub), 5, "atomic_rmw_sub"),
        (AtomicRmw(AtomicRmwOp::And), 5, "atomic_rmw_and"),
        (AtomicRmw(AtomicRmwOp::Or), 5, "atomic_rmw_or"),
        (AtomicRmw(AtomicRmwOp::Xor), 5, "atomic_rmw_xor"),
        (AtomicRmw(AtomicRmwOp::Nand), 5, "atomic_rmw_nand"),
        (AtomicRmw(AtomicRmwOp::Max), 5, "atomic_rmw_max"),
        (AtomicRmw(AtomicRmwOp::Min), 5, "atomic_rmw_min"),
        (AtomicRmw(AtomicRmwOp::UMax), 5, "atomic_rmw_umax"),
        (AtomicRmw(AtomicRmwOp::UMin), 5, "atomic_rmw_umin"),
        (CmpXchg, 8, "cmpxchg"),
        (Fence, 1, "fence"),
        (Gep, 3, "gep"),
        (CallNative, call_native_operand_contract().len() as u8, "call_native"),
        (Br, 1, "br"),
        (BrCond, 3, "br_if"),
        (VmCall, 1, "vm_call"),
        (VmRet, 0, "vm_ret"),
        (Ret, 1, "ret"),
    ];

    for (semantic, operands, name) in required {
        let instruction = profile
            .isa
            .by_semantic(&semantic)
            .ok_or_else(|| ProfileError::Invalid(format!("isa.vm must declare instruction {name}")))?;
        if instruction.operands != operands {
            return Err(ProfileError::Invalid(format!(
                "isa.vm instruction {name} must have {operands} operands, got {}",
                instruction.operands
            )));
        }
    }

    Ok(())
}

fn verify_lowering_rules(profile: &ProfilePackage) -> Result<(), ProfileError> {
    // Rust translator 刻意保持保守，但 profile 仍必须声明它声称能 lowering 的 LLVM 子集。
    // 这样可以防止 profile package 启用一个“可见 lowering 契约”窄于实际改写路径的 pass。
    let matchers: HashSet<_> = profile
        .lowering
        .rules
        .iter()
        .filter_map(|rule| rule.matcher.as_ref().map(|matcher| matcher.pattern.as_str()))
        .collect();
    for (contract, pattern) in REQUIRED_LOWERING_MATCHES {
        if !matchers.contains(pattern) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm must declare {contract} with match {pattern}"
            )));
        }
    }

    let isa_names: HashSet<_> = profile
        .isa
        .instructions
        .iter()
        .map(|instruction| instruction.name.as_str())
        .collect();
    let mut unique_matchers = HashSet::new();
    for rule in &profile.lowering.rules {
        let Some(matcher) = &rule.matcher else {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm rule {} must declare match",
                rule.name
            )));
        };
        if !unique_matchers.insert(matcher.pattern.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm match {} is declared by more than one rule",
                matcher.pattern
            )));
        }
        for emitted in &rule.emitted_instructions {
            if !isa_names.contains(emitted.as_str()) {
                return Err(ProfileError::Invalid(format!(
                    "lowering.vm rule {} emits instruction {emitted} but isa.vm does not declare it",
                    rule.name
                )));
            }
        }
        verify_lowering_action_flow(profile, rule)?;
    }

    verify_lowering_fusions(profile, &isa_names)?;

    Ok(())
}

fn verify_lowering_fusions(profile: &ProfilePackage, isa_names: &HashSet<&str>) -> Result<(), ProfileError> {
    let mut names = HashSet::new();
    let mut targets = HashSet::new();
    for fusion in &profile.lowering.fusions {
        if !names.insert(fusion.name.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm fusion {} is declared more than once",
                fusion.name
            )));
        }
        if !targets.insert(fusion.target.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm declares more than one fusion for target {}",
                fusion.target
            )));
        }
        let target = profile
            .isa
            .instructions
            .iter()
            .find(|desc| desc.name == fusion.target)
            .ok_or_else(|| {
                ProfileError::Invalid(format!(
                    "lowering.vm fusion {} targets {} but isa.vm does not declare it",
                    fusion.name, fusion.target
                ))
            })?;
        if !matches!(target.semantic, HandlerSemantic::Super(_)) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm fusion {} target {} is not a Super semantic instruction",
                fusion.name, fusion.target
            )));
        }
        for source in &fusion.sequence {
            if !isa_names.contains(source.as_str()) {
                return Err(ProfileError::Invalid(format!(
                    "lowering.vm fusion {} references source instruction {source} but isa.vm does not declare it",
                    fusion.name
                )));
            }
        }
        verify_supported_fusion_template(profile, &target.semantic, fusion)?;
    }

    for (super_op, name) in [
        (SuperOp::AddXor, "iadd_xor"),
        (SuperOp::IcmpBrIf, "icmp_br_if"),
        (SuperOp::GepLoad, "gep_load"),
        (SuperOp::LoadAdd, "load_iadd"),
    ] {
        if let Some(desc) = profile.isa.by_semantic(&HandlerSemantic::Super(super_op)) {
            if profile.lowering.fusion_for_target(&desc.name).is_none() {
                return Err(ProfileError::Invalid(format!(
                    "lowering.vm must declare fusion super.{name} for isa.vm instruction {}",
                    desc.name
                )));
            }
        }
    }

    Ok(())
}

fn verify_supported_fusion_template(
    profile: &ProfilePackage,
    semantic: &HandlerSemantic,
    fusion: &crate::profile::LoweringFusion,
) -> Result<(), ProfileError> {
    let (expected_sequence, expected_names, required) = match semantic {
        HandlerSemantic::Super(SuperOp::AddXor) => (
            &[HandlerSemantic::Bin(BinOp::Add), HandlerSemantic::Bin(BinOp::Xor)][..],
            &["iadd", "ixor"][..],
            &["adjacent", "no_label_between", "temp_single_use", "same_width"][..],
        ),
        HandlerSemantic::Super(SuperOp::IcmpBrIf) => (
            &[HandlerSemantic::Icmp, HandlerSemantic::BrCond][..],
            &["icmp", "br_if"][..],
            &["adjacent", "no_label_between", "temp_single_use"][..],
        ),
        HandlerSemantic::Super(SuperOp::GepLoad) => (
            &[HandlerSemantic::Gep, HandlerSemantic::Load][..],
            &["gep", "load"][..],
            &["adjacent", "no_label_between", "temp_single_use"][..],
        ),
        HandlerSemantic::Super(SuperOp::LoadAdd) => (
            &[HandlerSemantic::Load, HandlerSemantic::Bin(BinOp::Add)][..],
            &["load", "iadd"][..],
            &["adjacent", "no_label_between", "temp_single_use", "same_width"][..],
        ),
        other => {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm fusion {} target semantic {other:?} is not supported",
                fusion.name
            )));
        },
    };

    let sequence_semantics = fusion
        .sequence
        .iter()
        .map(|source| {
            profile
                .isa
                .instructions
                .iter()
                .find(|desc| desc.name == *source)
                .map(|desc| desc.semantic.clone())
                .ok_or_else(|| {
                    ProfileError::Invalid(format!(
                        "lowering.vm fusion {} references source instruction {source} but isa.vm does not declare it",
                        fusion.name
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if sequence_semantics != expected_sequence {
        return Err(ProfileError::Invalid(format!(
            "lowering.vm fusion {} sequence must have semantics {}",
            fusion.name,
            expected_names.join(", ")
        )));
    }

    let declared = fusion.requirements.iter().map(String::as_str).collect::<HashSet<_>>();
    for requirement in required {
        if !declared.contains(requirement) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm fusion {} must require {requirement}",
                fusion.name
            )));
        }
    }
    for requirement in &fusion.requirements {
        if !required.contains(&requirement.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm fusion {} declares unsupported requirement {requirement}",
                fusion.name
            )));
        }
    }

    Ok(())
}

fn verify_lowering_action_flow(
    profile: &ProfilePackage,
    rule: &crate::profile::LoweringRule,
) -> Result<(), ProfileError> {
    let mut vm_values = HashSet::new();
    let mut bound_llvm_values = HashSet::new();
    for action in &rule.actions {
        match action {
            LoweringAction::Materialize { target, .. } | LoweringAction::VReg { target, .. } => {
                vm_values.insert(target.as_str());
            },
            LoweringAction::Bind { llvm_value, vm_value } => {
                if !vm_values.contains(vm_value.as_str()) {
                    return Err(ProfileError::Invalid(format!(
                        "lowering.vm rule {} binds {llvm_value} to undefined VM value {vm_value}",
                        rule.name
                    )));
                }
                bound_llvm_values.insert(llvm_value.as_str());
            },
            LoweringAction::Emit { instruction, operands } => {
                let instruction_desc = profile
                    .isa
                    .instructions
                    .iter()
                    .find(|desc| desc.name == *instruction)
                    .ok_or_else(|| {
                        ProfileError::Invalid(format!(
                            "lowering.vm rule {} emits instruction {instruction} but isa.vm does not declare it",
                            rule.name
                        ))
                    })?;
                for (operand, expression) in operands {
                    if !instruction_desc.operand_descs.iter().any(|desc| desc.name == *operand) {
                        return Err(ProfileError::Invalid(format!(
                            "lowering.vm rule {} emits {} operand {operand} but isa.vm does not declare that operand",
                            rule.name, instruction_desc.name
                        )));
                    }
                    verify_lowering_emit_expression(rule, expression, &vm_values)?;
                }
            },
        }
    }

    if let Some(required_bind) = required_lowering_bind(rule) {
        if !bound_llvm_values.contains(required_bind) {
            return Err(ProfileError::Invalid(format!(
                "lowering.vm rule {} must bind {required_bind} to a defined VM value",
                rule.name
            )));
        }
    }

    Ok(())
}

fn required_lowering_bind(rule: &crate::profile::LoweringRule) -> Option<&str> {
    let matcher = rule.matcher.as_ref()?;
    if matcher.pattern == "llvm.memory scalar %ptr" {
        return Some("%r");
    }

    let (lhs, _) = matcher.pattern.split_once('=')?;
    let result = lhs.trim();
    result.starts_with('%').then_some(result)
}

fn verify_lowering_emit_expression(
    rule: &crate::profile::LoweringRule,
    expression: &str,
    vm_values: &HashSet<&str>,
) -> Result<(), ProfileError> {
    let expression = expression.trim();
    if !expression.starts_with('%') {
        return Ok(());
    }
    if vm_values.contains(expression) {
        return Ok(());
    }

    Err(ProfileError::Invalid(format!(
        "lowering.vm rule {} emits undefined VM value {expression}",
        rule.name
    )))
}

fn verify_bytecode(profile: &ProfilePackage) -> Result<(), ProfileError> {
    for (name, expected) in [
        ("header", SegmentMode::Fixed),
        ("const_pool", SegmentMode::Fixed),
        ("code", SegmentMode::Compressed),
        ("reloc", SegmentMode::Fixed),
    ] {
        let segment = profile
            .bytecode
            .segment(name)
            .ok_or_else(|| ProfileError::Invalid(format!("bytecode.vm must declare segment {name}")))?;
        if segment.mode != expected {
            return Err(ProfileError::Invalid(format!(
                "bytecode.vm segment {name} must be {expected:?}, got {:?}",
                segment.mode
            )));
        }
    }

    for segment in &profile.bytecode.segments {
        match segment.name.as_str() {
            "header" | "const_pool" | "code" | "reloc" => {},
            other => {
                return Err(ProfileError::Invalid(format!(
                    "bytecode.vm declares unsupported segment {other}"
                )));
            },
        }
    }

    if profile.bytecode.code_segment != SegmentMode::Compressed {
        return Err(ProfileError::Invalid(
            "bytecode.vm segment code must be compressed".to_owned(),
        ));
    }

    if profile.bytecode.instruction_record.opcode != OpcodeEncoding::VarintEncrypted {
        return Err(ProfileError::Invalid(
            "bytecode.vm record instr opcode must be varint encrypted".to_owned(),
        ));
    }
    match &profile.bytecode.instruction_record.operands {
        OperandEncoding::Bitpack { schema } if schema == "operand_stream" => {},
        OperandEncoding::Bitpack { schema } => {
            return Err(ProfileError::Invalid(format!(
                "bytecode.vm record instr operands must use schema=operand_stream, got {schema}"
            )));
        },
    }

    let reloc = profile
        .bytecode
        .relocation("label_pc")
        .ok_or_else(|| ProfileError::Invalid("bytecode.vm must declare reloc label_pc".to_owned()))?;
    if reloc.width != RelocWidth::Varint {
        return Err(ProfileError::Invalid(
            "bytecode.vm reloc label_pc width must be varint".to_owned(),
        ));
    }
    if reloc.base != RelocBase::CodeStart {
        return Err(ProfileError::Invalid(
            "bytecode.vm reloc label_pc base must be code_start".to_owned(),
        ));
    }

    if profile.bytecode.const_pool.encryption != ConstPoolEncryption::XorStreamFunctionKey {
        return Err(ProfileError::Invalid(
            "bytecode.vm const_pool encryption must be xor_stream key=function_key".to_owned(),
        ));
    }

    for (name, enabled, count) in [
        (
            "fake_instruction",
            profile.bytecode.fake_instruction.enabled,
            profile.bytecode.fake_instruction.count,
        ),
        (
            "dead_bytecode",
            profile.bytecode.dead_bytecode.enabled,
            profile.bytecode.dead_bytecode.count,
        ),
    ] {
        if enabled && count == 0 {
            return Err(ProfileError::Invalid(format!(
                "bytecode.vm {name} must set count > 0 when enabled"
            )));
        }
    }

    if (profile.bytecode.fake_instruction.enabled || profile.bytecode.dead_bytecode.enabled)
        && profile
            .isa
            .by_semantic(&HandlerSemantic::Nop)
            .is_none_or(|instruction| instruction.operands != 0)
    {
        return Err(ProfileError::Invalid(
            "isa.vm must declare a zero-operand Nop instruction for fake/dead bytecode".to_owned(),
        ));
    }
    Ok(())
}

fn is_valid_register(register: &str) -> bool {
    if let Some(index) = register.strip_prefix('x').and_then(|v| v.parse::<u8>().ok()) {
        index < 32
    } else if let Some(index) = register.strip_prefix('q').and_then(|v| v.parse::<u8>().ok()) {
        index < 65
    } else {
        false
    }
}

fn is_q_register(register: &str) -> bool {
    register
        .strip_prefix('q')
        .and_then(|v| v.parse::<u8>().ok())
        .is_some_and(|index| index < 65)
}

fn verify_decoder_steps(steps: &[DecoderStep]) -> Result<(), ProfileError> {
    let varint = steps.iter().position(|step| *step == DecoderStep::VarintDecode);
    let bit_unpack = steps.iter().position(|step| *step == DecoderStep::BitUnpack);

    let Some(varint) = varint else {
        return Err(ProfileError::Invalid(
            "decoder.vm must include varint_decode".to_owned(),
        ));
    };
    let Some(bit_unpack) = bit_unpack else {
        return Err(ProfileError::Invalid("decoder.vm must include bit_unpack".to_owned()));
    };

    if varint >= bit_unpack {
        return Err(ProfileError::Invalid(
            "decoder.vm varint_decode must run before bit_unpack".to_owned(),
        ));
    }

    for (index, step) in steps.iter().enumerate() {
        match step {
            DecoderStep::Rol { amount } | DecoderStep::Ror { amount } => {
                if !(1..=7).contains(amount) {
                    return Err(ProfileError::Invalid(format!(
                        "decoder.vm rotate amount must be in 1..=7, got {amount}"
                    )));
                }
            },
            DecoderStep::XorStream | DecoderStep::AddStream => {
                if index > varint {
                    return Err(ProfileError::Invalid(
                        "decoder.vm byte stream transforms must run before varint_decode".to_owned(),
                    ));
                }
            },
            DecoderStep::VarintDecode | DecoderStep::BitUnpack => {},
        }
        if index > varint && !matches!(step, DecoderStep::BitUnpack) {
            return Err(ProfileError::Invalid(
                "decoder.vm only bit_unpack may appear after varint_decode".to_owned(),
            ));
        }
    }

    if !steps[..varint].iter().any(|step| {
        matches!(
            step,
            DecoderStep::XorStream | DecoderStep::AddStream | DecoderStep::Rol { .. } | DecoderStep::Ror { .. }
        )
    }) {
        return Err(ProfileError::Invalid(
            "decoder.vm must include at least one reversible byte stream transform".to_owned(),
        ));
    }

    Ok(())
}

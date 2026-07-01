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
use crate::isa::{BinOp, CastOp, HandlerSemantic, OperandKind};
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

fn expected_operands(semantic: &HandlerSemantic) -> Vec<(String, OperandKind)> {
    use HandlerSemantic::*;
    use OperandKind::*;

    match semantic {
        MovImm => operands([("dst", VReg), ("imm", Imm), ("width", Imm)]),
        ConstLoad => operands([("dst", VReg), ("index", ConstPoolIndex), ("width", Imm)]),
        Mov => operands([("dst", VReg), ("src", VReg), ("width", Imm)]),
        Bin(_) => operands([("dst", VReg), ("lhs", VReg), ("rhs", VReg), ("width", Imm)]),
        Icmp => operands([
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
    use HandlerSemantic::*;

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
        (Icmp, 5, "icmp"),
        (Cast(ZExt), 4, "zext"),
        (Cast(SExt), 4, "sext"),
        (Cast(Trunc), 4, "trunc"),
        (Cast(Bitcast), 4, "bitcast"),
        (Alloca, 3, "alloca"),
        (Load, 3, "load"),
        (Store, 3, "store"),
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

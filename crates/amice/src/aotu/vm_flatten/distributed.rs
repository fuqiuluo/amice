use super::{
    done_attribute_name,
    index_program::IndexProgram,
    phi_transport::{PhiCodec, PhiInput, PhiRewrite},
    sbox::SBox,
    target_table::{AddressProgram, DecodeMode, StateProgram, TARGET_VARIANTS, TargetResolverAbi, TargetTable},
};
use crate::config::VmFlattenConfig;
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt, VerifyResult};
use amice_plugin::inkwell::{
    AddressSpace, IntPredicate,
    attributes::{Attribute, AttributeLoc},
    basic_block::BasicBlock,
    builder::Builder,
    comdat::ComdatSelectionKind,
    llvm_sys::core::{LLVMBuildFreeze, LLVMDeleteBasicBlock, LLVMGetOperand, LLVMSetOperand},
    module::{Linkage, Module},
    types::IntType,
    values::{
        AsValueRef, BasicValue, FunctionValue, InstructionOpcode as Op, InstructionValue, IntValue, PointerValue,
    },
};
use anyhow::{Context, ensure};
use rand::{Rng, SeedableRng, rngs::StdRng, seq::SliceRandom};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;

const STATE_LANES: usize = 3;
const MAX_WITNESSES: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateTiming {
    BeforeTarget,
    AfterTarget,
}

#[derive(Clone, Copy)]
enum BytecodeSelector {
    LowBits,
    Folded { shift: u32 },
    Rotated { shift: u32 },
    Affine { multiplier: u32, salt: u32, shift: u32 },
}

impl BytecodeSelector {
    fn generate(rng: &mut impl Rng) -> Self {
        match rng.random_range(0..4) {
            0 => Self::LowBits,
            1 => Self::Folded {
                shift: rng.random_range(1..32),
            },
            2 => Self::Rotated {
                shift: rng.random_range(1..32),
            },
            _ => Self::Affine {
                multiplier: rng.random::<u32>() | 1,
                salt: rng.random(),
                shift: rng.random_range(0..31),
            },
        }
    }

    fn emit<'ctx>(self, builder: &Builder<'ctx>, state: IntValue<'ctx>, name: &str) -> anyhow::Result<IntValue<'ctx>> {
        let word = state.get_type();
        let mixed = match self {
            Self::LowBits => state,
            Self::Folded { shift } => {
                let shifted = builder.build_right_shift(state, word.const_int(u64::from(shift), false), false, name)?;
                builder.build_xor(state, shifted, name)?
            },
            Self::Rotated { shift } => {
                let left = builder.build_left_shift(state, word.const_int(u64::from(shift), false), name)?;
                let right =
                    builder.build_right_shift(state, word.const_int(u64::from(32 - shift), false), false, name)?;
                builder.build_or(left, right, name)?
            },
            Self::Affine {
                multiplier,
                salt,
                shift,
            } => {
                let multiplied = builder.build_int_mul(state, word.const_int(u64::from(multiplier), false), name)?;
                let salted = builder.build_int_add(multiplied, word.const_int(u64::from(salt), false), name)?;
                builder.build_right_shift(salted, word.const_int(u64::from(shift), false), false, name)?
            },
        };
        Ok(builder.build_and(mixed, word.const_int(3, false), name)?)
    }
}

#[derive(Clone, Copy)]
enum BytecodeEnvelope {
    Affine {
        selector: BytecodeSelector,
        multipliers: [u32; 4],
        inverses: [u32; 4],
        adds: [u32; 4],
    },
    RotateXor {
        selector: BytecodeSelector,
        rotations: [u32; 4],
        masks: [u32; 4],
    },
    AddXor {
        selector: BytecodeSelector,
        adds: [u32; 4],
        masks: [u32; 4],
    },
}

impl BytecodeEnvelope {
    fn generate(rng: &mut impl Rng) -> Self {
        let selector = BytecodeSelector::generate(rng);
        match rng.random_range(0..3) {
            0 => {
                let mut multipliers = [0u32; 4];
                let mut inverses = [0u32; 4];
                let mut adds = [0u32; 4];
                for ((multiplier, inverse), add) in multipliers.iter_mut().zip(&mut inverses).zip(&mut adds) {
                    *multiplier = rng.random::<u32>() | 1;
                    *inverse = inverse_odd_u32(*multiplier);
                    *add = rng.random();
                }
                Self::Affine {
                    selector,
                    multipliers,
                    inverses,
                    adds,
                }
            },
            1 => Self::RotateXor {
                selector,
                rotations: std::array::from_fn(|_| rng.random_range(1..32)),
                masks: std::array::from_fn(|_| rng.random()),
            },
            _ => Self::AddXor {
                selector,
                adds: std::array::from_fn(|_| rng.random()),
                masks: std::array::from_fn(|_| rng.random()),
            },
        }
    }

    fn select<'ctx>(
        builder: &Builder<'ctx>,
        selector: IntValue<'ctx>,
        values: &[u32; 4],
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = selector.get_type();
        let mut selected = word.const_int(u64::from(values[0]), false);
        for (index, value) in values.iter().enumerate().skip(1) {
            let matches = builder.build_int_compare(
                IntPredicate::EQ,
                selector,
                word.const_int(u64::try_from(index)?, false),
                name,
            )?;
            selected = builder
                .build_select(matches, word.const_int(u64::from(*value), false), selected, name)?
                .into_int_value();
        }
        Ok(selected)
    }

    fn key<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        state: IntValue<'ctx>,
        values: &[u32; 4],
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let selector = match self {
            Self::Affine { selector, .. } | Self::RotateXor { selector, .. } | Self::AddXor { selector, .. } => {
                selector.emit(builder, state, name)?
            },
        };
        Self::select(builder, selector, values, name)
    }

    fn rotate_left<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        shift: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let opposite = builder.build_int_sub(word.const_int(32, false), shift, name)?;
        let left = builder.build_left_shift(value, shift, name)?;
        let right = builder.build_right_shift(value, opposite, false, name)?;
        Ok(builder.build_or(left, right, name)?)
    }

    fn rotate_right<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        shift: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let opposite = builder.build_int_sub(word.const_int(32, false), shift, name)?;
        let right = builder.build_right_shift(value, shift, false, name)?;
        let left = builder.build_left_shift(value, opposite, name)?;
        Ok(builder.build_or(left, right, name)?)
    }

    fn encode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        match self {
            Self::Affine { multipliers, adds, .. } => {
                let multiplier = self.key(builder, state, multipliers, name)?;
                let add = self.key(builder, state, adds, name)?;
                let mixed = builder.build_int_mul(value, multiplier, name)?;
                Ok(builder.build_int_add(mixed, add, name)?)
            },
            Self::RotateXor { rotations, masks, .. } => {
                let rotation = self.key(builder, state, rotations, name)?;
                let mask = self.key(builder, state, masks, name)?;
                let rotated = Self::rotate_left(builder, value, rotation, name)?;
                Ok(builder.build_xor(rotated, mask, name)?)
            },
            Self::AddXor { adds, masks, .. } => {
                let add = self.key(builder, state, adds, name)?;
                let mask = self.key(builder, state, masks, name)?;
                let added = builder.build_int_add(value, add, name)?;
                Ok(builder.build_xor(added, mask, name)?)
            },
        }
    }

    fn decode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        match self {
            Self::Affine { inverses, adds, .. } => {
                let inverse = self.key(builder, state, inverses, name)?;
                let add = self.key(builder, state, adds, name)?;
                let unadded = builder.build_int_sub(value, add, name)?;
                Ok(builder.build_int_mul(unadded, inverse, name)?)
            },
            Self::RotateXor { rotations, masks, .. } => {
                let rotation = self.key(builder, state, rotations, name)?;
                let mask = self.key(builder, state, masks, name)?;
                let unmasked = builder.build_xor(value, mask, name)?;
                Ok(Self::rotate_right(builder, unmasked, rotation, name)?)
            },
            Self::AddXor { adds, masks, .. } => {
                let add = self.key(builder, state, adds, name)?;
                let mask = self.key(builder, state, masks, name)?;
                let unmasked = builder.build_xor(value, mask, name)?;
                Ok(builder.build_int_sub(unmasked, add, name)?)
            },
        }
    }

    #[cfg(test)]
    fn host_selector(selector: BytecodeSelector, state: u32) -> usize {
        let mixed = match selector {
            BytecodeSelector::LowBits => state,
            BytecodeSelector::Folded { shift } => state ^ (state >> shift),
            BytecodeSelector::Rotated { shift } => state.rotate_left(shift),
            BytecodeSelector::Affine {
                multiplier,
                salt,
                shift,
            } => state.wrapping_mul(multiplier).wrapping_add(salt) >> shift,
        };
        usize::try_from(mixed & 3).expect("selector is two bits")
    }

    #[cfg(test)]
    fn host_encode(self, value: u32, state: u32) -> u32 {
        match self {
            Self::Affine {
                selector,
                multipliers,
                adds,
                ..
            } => value
                .wrapping_mul(multipliers[Self::host_selector(selector, state)])
                .wrapping_add(adds[Self::host_selector(selector, state)]),
            Self::RotateXor {
                selector,
                rotations,
                masks,
            } => {
                let index = Self::host_selector(selector, state);
                value.rotate_left(rotations[index]) ^ masks[index]
            },
            Self::AddXor { selector, adds, masks } => {
                let index = Self::host_selector(selector, state);
                value.wrapping_add(adds[index]) ^ masks[index]
            },
        }
    }

    #[cfg(test)]
    fn host_decode(self, value: u32, state: u32) -> u32 {
        match self {
            Self::Affine {
                selector,
                inverses,
                adds,
                ..
            } => {
                let index = Self::host_selector(selector, state);
                value.wrapping_sub(adds[index]).wrapping_mul(inverses[index])
            },
            Self::RotateXor {
                selector,
                rotations,
                masks,
            } => {
                let index = Self::host_selector(selector, state);
                (value ^ masks[index]).rotate_right(rotations[index])
            },
            Self::AddXor { selector, adds, masks } => {
                let index = Self::host_selector(selector, state);
                (value ^ masks[index]).wrapping_sub(adds[index])
            },
        }
    }
}

fn inverse_odd_u32(value: u32) -> u32 {
    let mut inverse = 1u32;
    for _ in 0..5 {
        inverse = inverse.wrapping_mul(2u32.wrapping_sub(value.wrapping_mul(inverse)));
    }
    inverse
}

#[derive(Clone, Copy)]
enum GuardStyle {
    Select,
    Affine,
    Masked,
}

/// Reversible authentication envelopes selected by the row-record tag.
/// The decoded tag is checked at the destination gateway.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum GatewayCodec {
    Xor,
    Add,
    Sub,
    RotatedXor { shift: u32 },
}

impl GatewayCodec {
    fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        match rng.random_range(0..4) {
            0 => Self::Xor,
            1 => Self::Add,
            2 => Self::Sub,
            _ => Self::RotatedXor {
                shift: rng.random_range(1..bits),
            },
        }
    }

    fn rotate<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        shift: u32,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let left = builder.build_left_shift(value, word.const_int(u64::from(shift), false), name)?;
        let right = builder.build_right_shift(
            value,
            word.const_int(u64::from(word.get_bit_width() - shift), false),
            false,
            name,
        )?;
        Ok(builder.build_or(left, right, name)?)
    }

    fn mix_state<'ctx>(
        self,
        builder: &Builder<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        match self {
            Self::Xor => Ok(state),
            Self::Add | Self::Sub => Ok(state),
            Self::RotatedXor { shift } => Self::rotate(builder, state, shift, name),
        }
    }

    fn encode<'ctx>(
        self,
        builder: &Builder<'ctx>,
        state: IntValue<'ctx>,
        tag: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let state = self.mix_state(builder, state, name)?;
        match self {
            Self::Xor | Self::RotatedXor { .. } => Ok(builder.build_xor(state, tag, name)?),
            Self::Add => Ok(builder.build_int_add(state, tag, name)?),
            Self::Sub => Ok(builder.build_int_sub(state, tag, name)?),
        }
    }

    fn decode<'ctx>(
        self,
        builder: &Builder<'ctx>,
        token: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let state = self.mix_state(builder, state, name)?;
        match self {
            Self::Xor | Self::RotatedXor { .. } => Ok(builder.build_xor(token, state, name)?),
            Self::Add => Ok(builder.build_int_sub(token, state, name)?),
            Self::Sub => Ok(builder.build_int_sub(state, token, name)?),
        }
    }
}

impl GuardStyle {
    const ALL: [Self; 3] = [Self::Select, Self::Affine, Self::Masked];
}

#[derive(Clone, Copy)]
struct GuardConfig {
    style: GuardStyle,
    accept: u64,
    reject: u64,
    reject_mix: u64,
}

impl GuardConfig {
    fn generate(rng: &mut impl Rng, bits: u32, style: GuardStyle) -> Self {
        let mask = if bits == 32 { u64::from(u32::MAX) } else { u64::MAX };
        let reject = rng.random::<u64>() & mask;
        // Adjacent values make the affine and masked forms invertible while
        // remaining distinct at both supported target widths.
        let accept = reject ^ 1;
        // The loop alternates reject and reject ^ reject_mix. Bit 1 ensures
        // neither value equals accept, even when the target truncates to i32.
        let reject_mix = (rng.random::<u64>() | 2) & mask;
        Self {
            style,
            accept,
            reject,
            reject_mix,
        }
    }

    fn edge_assignments(rng: &mut impl Rng, edges: usize) -> Vec<usize> {
        let mut assignments: Vec<_> = (0..edges)
            .map(|edge| {
                if edge < GuardStyle::ALL.len() {
                    edge
                } else {
                    rng.random_range(0..GuardStyle::ALL.len())
                }
            })
            .collect();
        // Guarantee multiple encodings even for small functions without
        // making the first edges use a predictable sequence of call targets.
        assignments.shuffle(rng);
        assignments
    }

    fn emit_marker<'ctx>(
        self,
        builder: &Builder<'ctx>,
        changed: IntValue<'ctx>,
        word: IntType<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        match self.style {
            GuardStyle::Select => Ok(builder
                .build_select(
                    changed,
                    word.const_int(self.accept, false),
                    word.const_int(self.reject, false),
                    name,
                )?
                .into_int_value()),
            GuardStyle::Affine => {
                let bit = builder.build_int_z_extend(changed, word, name)?;
                let scaled =
                    builder.build_int_mul(bit, word.const_int(self.accept.wrapping_sub(self.reject), false), name)?;
                Ok(builder.build_int_add(scaled, word.const_int(self.reject, false), name)?)
            },
            GuardStyle::Masked => {
                let bit = builder.build_int_z_extend(changed, word, name)?;
                Ok(builder.build_xor(bit, word.const_int(self.reject, false), name)?)
            },
        }
    }

    fn emit_check<'ctx>(
        self,
        builder: &Builder<'ctx>,
        loaded: IntValue<'ctx>,
        word: IntType<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        match self.style {
            GuardStyle::Select => {
                Ok(builder.build_int_compare(IntPredicate::EQ, loaded, word.const_int(self.accept, false), name)?)
            },
            GuardStyle::Affine => {
                let difference = builder.build_int_sub(loaded, word.const_int(self.accept, false), name)?;
                Ok(builder.build_int_compare(IntPredicate::EQ, difference, word.const_zero(), name)?)
            },
            GuardStyle::Masked => {
                let masked = builder.build_xor(loaded, word.const_int(self.reject, false), name)?;
                Ok(builder.build_int_compare(IntPredicate::EQ, masked, word.const_int(1, false), name)?)
            },
        }
    }
    fn create_sink<'ctx>(
        self,
        module: &Module<'ctx>,
        address_word: IntType<'ctx>,
        rng: &mut impl Rng,
        value_name: &str,
    ) -> anyhow::Result<FunctionValue<'ctx>> {
        let ctx = module.get_context();
        let sink_name = format!("v{:016x}", rng.random::<u64>());
        let sink_type = ctx
        .void_type()
        // A variadic private helper is deliberately outside BCF's forwarding
        // subset. It keeps a later BCF pass from cloning the invalid-path sink
        // when module passes are repeated.
        .fn_type(&[ctx.ptr_type(AddressSpace::default()).into()], true);
        let sink = module.add_function(&sink_name, sink_type, Some(Linkage::Private));
        let result = (|| -> anyhow::Result<()> {
            let sink_done = done_attribute_name(sink);
            sink.add_attribute(AttributeLoc::Function, ctx.create_string_attribute(&sink_done, "1"));
            for attribute in ["noinline", "optnone"] {
                sink.add_attribute(
                    AttributeLoc::Function,
                    ctx.create_enum_attribute(Attribute::get_named_enum_kind_id(attribute), 0),
                );
            }
            let sink_entry = ctx.append_basic_block(sink, &format!("v{:016x}", rng.random::<u64>()));
            let sink_loop = ctx.append_basic_block(sink, &format!("v{:016x}", rng.random::<u64>()));
            let sink_mix = ctx.append_basic_block(sink, &format!("v{:016x}", rng.random::<u64>()));
            let sink_done = ctx.append_basic_block(sink, &format!("v{:016x}", rng.random::<u64>()));
            let sink_ptr = sink
                .get_first_param()
                .context("VM sink has no pointer parameter")?
                .into_pointer_value();
            let sink_builder = ctx.create_builder();
            sink_builder.position_at_end(sink_entry);
            sink_builder.build_unconditional_branch(sink_loop)?;
            sink_builder.position_at_end(sink_loop);
            let sink_value = sink_builder
                .build_load2(address_word, sink_ptr, value_name)?
                .into_int_value();
            sink_value
                .as_instruction_value()
                .context("VM sink load is not a load")?
                .set_volatile(true)?;
            // Each sink has a distinct encoded-valid marker. Continuations select a
            // sink independently, so one function no longer exposes one universal
            // postcondition signature.
            let sink_valid = self.emit_check(&sink_builder, sink_value, address_word, value_name)?;
            sink_builder.build_conditional_branch(sink_valid, sink_done, sink_mix)?;

            sink_builder.position_at_end(sink_mix);
            let sink_value = sink_builder
                .build_load2(address_word, sink_ptr, value_name)?
                .into_int_value();
            sink_value
                .as_instruction_value()
                .context("VM sink mix load is not a load")?
                .set_volatile(true)?;
            let sink_mixed =
                sink_builder.build_xor(sink_value, address_word.const_int(self.reject_mix, false), value_name)?;
            sink_builder.build_store(sink_ptr, sink_mixed)?.set_volatile(true)?;
            sink_builder.build_unconditional_branch(sink_loop)?;
            sink_builder.position_at_end(sink_done);
            sink_builder.build_return(None)?;
            Ok(())
        })();
        if let Err(error) = result {
            // SAFETY: Construction failed before any caller could reference the
            // private sink, so its blocks and instructions can be removed here.
            unsafe { sink.delete() };
            return Err(error);
        }
        Ok(sink)
    }
}

impl StateTiming {
    fn generate(rng: &mut impl Rng) -> Self {
        if rng.random_bool(0.5) {
            Self::BeforeTarget
        } else {
            Self::AfterTarget
        }
    }
}

struct Transfer<'ctx, const PROGRAM_VARIANTS: usize> {
    block: BasicBlock<'ctx>,
    terminator: InstructionValue<'ctx>,
    // Retain edge multiplicity, including repeated switch destinations. This
    // preserves PHI incoming counts as well as predecessors and dominance.
    successors: Vec<BasicBlock<'ctx>>,
    phi_inputs: Vec<Vec<PhiInput<'ctx>>>,
    // Each transfer carries several independent reversible opcode programs.
    // The runtime schedule chooses one at every operation, so a static
    // extractor cannot normalize a row without first recovering its
    // program-family selector.
    programs: [IndexProgram; PROGRAM_VARIANTS],
    // Do not reuse one nonlinear network for the whole function. A shared
    // S-box gives a static extractor a single normal form to learn and match
    // across every transfer; each program variant therefore owns its own
    // transfer-local S-box families.
    substitutions: [[SBox; 3]; PROGRAM_VARIANTS],
    // The encoded edge value is bound to the invocation and transfer state
    // before the index program runs. This removes the direct constant-to-index
    // path while preserving a reversible 32-bit domain.
    bytecode_salt: u32,
    // A second, state-selected reversible envelope makes the stored edge
    // value depend on a runtime family rather than one universal XOR mask.
    bytecode_envelope: BytecodeEnvelope,
    // Do not derive every opcode key with the same add/xor expression. Each
    // transfer gets an independent envelope-shaped schedule, so the key
    // material remains invocation/state-bound while the emitted IR has no
    // single normal form for static recognizers to match across rows.
    opcode_schedule: BytecodeEnvelope,
    address_programs: Vec<AddressProgram>,
    state_programs: Vec<StateProgram>,
    digest_salt: u64,
    digest_rotate: u32,
    // A small vector of pre-existing integer SSA values couples the next state
    // to real program data without changing the current transfer's target. The
    // values are collected before any VM instructions are inserted, so
    // generated state operations can never become their own witnesses.
    witnesses: Vec<IntValue<'ctx>>,
    state_timing: StateTiming,
    decode_mode: DecodeMode,
    // The target record deliberately has more physical slots than logical
    // successor edges.  The current state chooses an alias for the same
    // gateway, so a recovered record row no longer gives a one-to-one edge
    // number -> target relation.  Aliases reuse existing gateways; they do
    // not add CFG predecessors and therefore cannot invalidate PHI dominance.
    slot_aliases: u32,
    // Four decoder envelopes are shared by the row. An edge's random record
    // tag chooses one envelope, so the source transfer does not contain a
    // direct successor-index-to-codec chain.
    gateway_codecs: Vec<GatewayCodec>,
}

fn eligible(function: FunctionValue<'_>) -> bool {
    let done = done_attribute_name(function);
    if function.count_basic_blocks() < 2
        || function.has_personality_function()
        || function.get_string_attribute(AttributeLoc::Function, &done).is_some()
        || function
            .get_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id("naked"))
            .is_some()
        || function.as_global_value().get_comdat().is_some_and(|comdat| {
            matches!(
                comdat.get_selection_kind(),
                ComdatSelectionKind::ExactMatch | ComdatSelectionKind::SameSize
            )
        })
    {
        return false;
    }
    function.get_basic_blocks().iter().all(|block| {
        !block.has_address_taken()
            && !block.is_eh_pad()
            && block
                .get_terminator()
                .is_some_and(|term| matches!(term.get_opcode(), Op::Br | Op::Switch | Op::Return | Op::Unreachable))
    })
}

fn successors(terminator: InstructionValue<'_>) -> anyhow::Result<Vec<BasicBlock<'_>>> {
    match terminator.get_opcode() {
        Op::Br => {
            let branch = terminator.into_branch_inst();
            let mut result = vec![branch.get_successor(0).context("branch without destination")?];
            if terminator.is_conditional()? {
                result.push(
                    branch
                        .get_successor(1)
                        .context("conditional branch without false destination")?,
                );
            }
            Ok(result)
        },
        Op::Switch => {
            let switch = terminator.into_switch_inst();
            Ok(std::iter::once(switch.get_default_block())
                .chain(switch.get_cases().into_iter().map(|(_, block)| block))
                .collect())
        },
        _ => Ok(Vec::new()),
    }
}

fn integer_witnesses<'ctx>(block: BasicBlock<'ctx>, terminator: InstructionValue<'ctx>) -> Vec<IntValue<'ctx>> {
    let mut wide = Vec::new();
    let mut narrow = Vec::new();
    let instructions: Vec<_> = block.get_instructions().collect();
    for instruction in instructions.into_iter().rev() {
        if instruction.as_value_ref() == terminator.as_value_ref() {
            continue;
        }
        let Ok(value) = IntValue::try_from(instruction) else {
            continue;
        };
        // Prefer values wider than a branch predicate. One-bit values remain
        // valid fallbacks for control-only blocks. Keep the source order
        // deterministic so an explicit seed reproduces the same IR.
        if value.get_type().get_bit_width() > 1 {
            wide.push(value);
        } else {
            narrow.push(value);
        }
    }
    wide.extend(narrow);
    wide.truncate(MAX_WITNESSES);
    wide
}

fn freeze_witness<'ctx>(builder: &Builder<'ctx>, witness: IntValue<'ctx>, name: &str) -> IntValue<'ctx> {
    // A live SSA integer may carry poison or undef even when the original
    // terminator never consumes it as control. Freeze makes the VM state
    // transition defined without changing the original value's uses.
    // SAFETY: The builder has a live insertion point and the operand is an
    // integer in the same LLVM context.
    let name = CString::new(name).expect("opaque VM value name has no NUL");
    unsafe {
        IntValue::new(LLVMBuildFreeze(
            builder.as_mut_ptr(),
            witness.as_value_ref(),
            name.as_ptr().cast(),
        ))
    }
}

/// Emit a private, non-inlined continuation helper used after gateway
/// authentication. It consumes invocation-local state and token cells, takes
/// two data-dependent paths, and converges before returning. The helper is
/// deliberately created after BCF has run, so the valid continuation is not a
/// bare `gateway -> original target` edge that a structural pass can fold
/// immediately. Besides the token scratch cell, it mutates the first VM state
/// lane. That write is consumed by the next transfer, so the helper is part of
/// the decoder state transition rather than an otherwise dead arithmetic
/// island.
fn create_continuation_helper<'ctx>(
    module: &Module<'ctx>,
    word: IntType<'ctx>,
    rng: &mut impl Rng,
) -> anyhow::Result<FunctionValue<'ctx>> {
    let ctx = module.get_context();
    let pointer = ctx.ptr_type(AddressSpace::default());
    let function_type = ctx
        .void_type()
        // Variadic private helpers stay outside BCF's forwarding/cloning
        // subset when a transformed module is processed again.
        .fn_type(&[pointer.into(), pointer.into()], true);
    let function = module.add_function(
        &format!("v{:016x}", rng.random::<u64>()),
        function_type,
        Some(Linkage::Private),
    );
    let done = done_attribute_name(function);
    function.add_attribute(AttributeLoc::Function, ctx.create_string_attribute(&done, "1"));
    // The helper is emitted after the normal BCF pass.  Mark it as already
    // handled so a later/repeated BCF pipeline cannot rewrite its private
    // arithmetic region and make the overall transform non-deterministic.
    function.add_attribute(
        AttributeLoc::Function,
        ctx.create_string_attribute("amice.bcf.done", "1"),
    );
    for attribute in ["noinline", "optnone"] {
        function.add_attribute(
            AttributeLoc::Function,
            ctx.create_enum_attribute(Attribute::get_named_enum_kind_id(attribute), 0),
        );
    }

    let entry = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
    let left = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
    let right = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
    let join = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
    let token = function
        .get_nth_param(0)
        .context("VM continuation helper has no token parameter")?
        .into_pointer_value();
    let state = function
        .get_nth_param(1)
        .context("VM continuation helper has no state parameter")?
        .into_pointer_value();
    let builder = ctx.create_builder();
    builder.position_at_end(entry);
    let token_value = builder.build_load2(word, token, "")?.into_int_value();
    token_value
        .as_instruction_value()
        .context("VM continuation token load")?
        .set_volatile(true)?;
    let state_value = builder.build_load2(word, state, "")?.into_int_value();
    state_value
        .as_instruction_value()
        .context("VM continuation state load")?
        .set_volatile(true)?;
    let guard = builder.build_int_compare(
        IntPredicate::ULT,
        state_value,
        word.const_int(rng.random::<u64>() | 1, false),
        "",
    )?;
    builder.build_conditional_branch(guard, left, right)?;

    builder.position_at_end(left);
    let left_value = builder.build_xor(token_value, state_value, "")?;
    let left_value = builder.build_int_add(left_value, word.const_int(rng.random(), false), "")?;
    builder.build_store(token, left_value)?.set_volatile(true)?;
    // Add/subtract one is deliberately non-identity for every 32/64-bit
    // state value. The caller verifies this postcondition after returning, so
    // removing the helper changes which path can reach the original target.
    let left_state = builder.build_int_add(state_value, word.const_int(1, false), "")?;
    builder.build_store(state, left_state)?.set_volatile(true)?;
    builder.build_unconditional_branch(join)?;

    builder.position_at_end(right);
    let right_value = builder.build_int_sub(token_value, state_value, "")?;
    let right_value = builder.build_xor(right_value, word.const_int(rng.random(), false), "")?;
    builder.build_store(token, right_value)?.set_volatile(true)?;
    let right_state = builder.build_int_sub(state_value, word.const_int(1, false), "")?;
    builder.build_store(state, right_state)?.set_volatile(true)?;
    builder.build_unconditional_branch(join)?;

    builder.position_at_end(join);
    builder.build_return(None)?;
    Ok(function)
}

fn state_digest<'ctx>(
    builder: &Builder<'ctx>,
    lanes: &[IntValue<'ctx>],
    salt: u64,
    rotate: u32,
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    ensure!(!lanes.is_empty(), "VM state has no lanes");
    let word = lanes[0].get_type();
    let bits = word.get_bit_width();
    let mut digest = lanes[0];
    for (index, lane) in lanes.iter().enumerate().skip(1) {
        let index = u32::try_from(index).expect("VM state lane count fits u32");
        let shift = ((rotate + index.saturating_mul(7)) % bits).max(1);
        let left = builder.build_left_shift(*lane, word.const_int(u64::from(shift), false), name)?;
        let right = builder.build_right_shift(*lane, word.const_int(u64::from(bits - shift), false), false, name)?;
        let rotated = builder.build_or(left, right, name)?;
        digest = builder.build_xor(digest, rotated, name)?;
        digest = builder.build_int_add(digest, word.const_int(salt.wrapping_add(u64::from(index)), false), name)?;
    }
    Ok(digest)
}

fn select_input<'ctx, const PROGRAM_VARIANTS: usize>(
    builder: &Builder<'ctx>,
    transfer: &Transfer<'ctx, PROGRAM_VARIANTS>,
    encoded: &[IntValue<'ctx>],
    name: &str,
) -> anyhow::Result<IntValue<'ctx>> {
    match transfer.terminator.get_opcode() {
        Op::Br if encoded.len() == 2 => {
            let condition = transfer
                .terminator
                .get_operand(0)
                .and_then(|operand| operand.value())
                .context("conditional branch without condition")?
                .into_int_value();
            Ok(builder
                .build_select(condition, encoded[0], encoded[1], name)?
                .into_int_value())
        },
        Op::Switch => {
            let switch = transfer.terminator.into_switch_inst();
            let condition = switch.get_condition().into_int_value();
            let mut selected = encoded[0];
            for ((value, _), input) in switch.get_cases().iter().zip(&encoded[1..]) {
                let matches = builder.build_int_compare(IntPredicate::EQ, condition, value.into_int_value(), name)?;
                selected = builder.build_select(matches, *input, selected, name)?.into_int_value();
            }
            Ok(selected)
        },
        _ => Ok(encoded[0]),
    }
}

pub(super) fn lower<'ctx>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    cfg: &VmFlattenConfig,
) -> anyhow::Result<bool> {
    match cfg.program_variants.clamp(1, 3) {
        1 => lower_with_variants::<1>(module, function, cfg),
        2 => lower_with_variants::<2>(module, function, cfg),
        _ => lower_with_variants::<3>(module, function, cfg),
    }
}

fn lower_with_variants<'ctx, const PROGRAM_VARIANTS: usize>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    cfg: &VmFlattenConfig,
) -> anyhow::Result<bool> {
    if !eligible(function) {
        log::debug!(
            "VM control-flow flattening: distributed mode skipped {:?}",
            function.get_name()
        );
        return Ok(false);
    }
    if let VerifyResult::Broken(message) = function.verify_function() {
        anyhow::bail!("invalid input to VM control-flow flattening: {message}");
    }
    let Some(address_word) = TargetTable::word_type(module, function) else {
        log::warn!(
            "VM control-flow flattening: {:?} requires integral 32/64-bit default-space pointers",
            function.get_name()
        );
        return Ok(false);
    };
    let blocks = function.get_basic_blocks();
    let entry = blocks[0];
    let seed = function.get_name().to_bytes().iter().fold(cfg.seed, |state, byte| {
        (state ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    let mut rng = StdRng::seed_from_u64(seed);
    let value_name = format!("v{:016x}", rng.random::<u64>());
    // Basic-block names are not semantic, but retaining names such as
    // `left`, `join` or `error` gives a recognizer an immediate view of the
    // original CFG. Rename the eligible function's original blocks before
    // collecting transfer metadata. LLVM keeps block handles and PHI edges
    // stable across this name-only change.
    let mut block_names = HashSet::with_capacity(blocks.len());
    for block in &blocks {
        let name = loop {
            let candidate = format!("b{:016x}", rng.random::<u64>());
            if block_names.insert(candidate.clone()) {
                break candidate;
            }
        };
        block.set_name(&name);
    }
    let targets: HashSet<_> = blocks[1..].iter().copied().collect();
    let mut transfers = Vec::new();
    for block in &blocks {
        let terminator = block.get_terminator().context("unterminated VM block")?;
        let successors = successors(terminator)?;
        if successors.is_empty() {
            continue;
        }
        ensure!(
            successors.iter().all(|bb| targets.contains(bb)),
            "VM destination outside non-entry blocks"
        );
        let mut occurrences = HashMap::new();
        let phi_inputs = successors
            .iter()
            .map(|target| {
                let occurrence = occurrences.entry(*target).or_insert(0);
                let inputs = PhiInput::on_edge(*target, *block, *occurrence)?;
                *occurrence += 1;
                Ok(inputs)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        // Keep one shared family of envelopes per transfer row. The record
        // tag chooses the envelope at runtime, so the source does not encode
        // a direct successor-index-to-codec mapping.
        let gateway_codecs = (0..TARGET_VARIANTS)
            .map(|_| GatewayCodec::generate(&mut rng, address_word.get_bit_width()))
            .collect();
        let bytecode_salt = rng.random();
        // A one-successor row already reveals no choice, so spending
        // duplicate record slots there only adds code size.  Reserve
        // aliases for rows whose logical edge set actually benefits from
        // decoupling physical slots from successor ordinals.
        let slot_aliases = if successors.len() > 1 {
            2 + (bytecode_salt & 1)
        } else {
            1
        };
        transfers.push(Transfer {
            block: *block,
            terminator,
            successors,
            phi_inputs,
            programs: IndexProgram::generate_variants::<PROGRAM_VARIANTS>(&mut rng, cfg.max_ops),
            substitutions: std::array::from_fn(|_| std::array::from_fn(|_| SBox::generate(&mut rng))),
            bytecode_salt,
            bytecode_envelope: BytecodeEnvelope::generate(&mut rng),
            opcode_schedule: BytecodeEnvelope::generate(&mut rng),
            address_programs: (0..TARGET_VARIANTS)
                .map(|_| AddressProgram::generate(&mut rng, address_word.get_bit_width()))
                .collect(),
            state_programs: (0..STATE_LANES)
                .map(|_| StateProgram::generate(&mut rng, address_word.get_bit_width()))
                .collect(),
            digest_salt: rng.random(),
            digest_rotate: rng.random_range(1..address_word.get_bit_width()),
            witnesses: integer_witnesses(*block, terminator),
            state_timing: StateTiming::generate(&mut rng),
            decode_mode: if rng.random_bool(0.5) {
                DecodeMode::CandidateSelect
            } else {
                DecodeMode::SharedProgram
            },
            // Derive the multiplicity from an already generated transfer
            // salt so adding aliases does not perturb every later random
            // lowering choice for an explicit seed.
            slot_aliases,
            gateway_codecs,
        });
    }
    if transfers.len() > 1 {
        let has_before = transfers
            .iter()
            .any(|transfer| matches!(transfer.state_timing, StateTiming::BeforeTarget));
        let has_after = transfers
            .iter()
            .any(|transfer| matches!(transfer.state_timing, StateTiming::AfterTarget));
        // Preserve the random choices in the normal case, but guarantee that
        // a multi-transfer function does not accidentally collapse to one
        // lowering family for a particular seed.
        if !has_before {
            transfers[0].state_timing = StateTiming::BeforeTarget;
        } else if !has_after {
            transfers[0].state_timing = StateTiming::AfterTarget;
        }
        let has_candidate_select = transfers
            .iter()
            .any(|transfer| matches!(transfer.decode_mode, DecodeMode::CandidateSelect));
        let has_shared_program = transfers
            .iter()
            .any(|transfer| matches!(transfer.decode_mode, DecodeMode::SharedProgram));
        // Keep both decoder envelopes present in a multi-row function. The
        // random assignment remains the normal choice; this only avoids a
        // particular seed accidentally collapsing to one static shape.
        if !has_candidate_select {
            transfers[0].decode_mode = DecodeMode::CandidateSelect;
        } else if !has_shared_program {
            transfers[0].decode_mode = DecodeMode::SharedProgram;
        }
    }
    if transfers.is_empty() {
        return Ok(false);
    }

    let ctx = module.get_context();
    let builder = ctx.create_builder();
    let word = ctx.i32_type();
    // Keep original instructions until verification succeeds, so a failed
    // rewrite can restore the function without cloning blockaddress constants.
    let original: HashSet<_> = blocks.iter().flat_map(|bb| bb.get_instructions()).collect();
    let mut detached = Vec::new();
    let mut decoy_blocks = Vec::new();
    let mut decoy_targets: Vec<Vec<BasicBlock<'ctx>>> = Vec::with_capacity(transfers.len());
    let mut gateway_blocks = Vec::new();
    let mut continuation_rows: Vec<Vec<BasicBlock<'ctx>>> = Vec::new();
    let mut gateway_tags: Vec<Vec<u64>> = Vec::new();
    let mut guard_sinks = Vec::new();
    let mut continuation_helpers = Vec::new();
    let mut continuation_edges: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>, BasicBlock<'ctx>, BasicBlock<'ctx>)> =
        Vec::new();
    let mut phi_rewrites: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>)> = Vec::new();
    let mut phi_values = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        // Give each source-to-target edge occurrence a synthetic landing block.
        // Duplicate switch edges therefore get distinct LLVM predecessors and
        // can retain distinct PHI incoming entries.
        builder.position_before(&entry.get_first_insertion_pt());
        let state = (0..STATE_LANES)
            .map(|_| builder.build_alloca(address_word, &value_name))
            .collect::<Result<Vec<_>, _>>()?;
        let continuation_choice = builder.build_alloca(ctx.bool_type(), &value_name)?;
        // The address of an invocation-local lane supplies a stable nonce for
        // this call. It is intentionally fed into each state transition so a
        // static recovery pass must model a runtime frame value in addition
        // to the public constants and SSA witness. Keep the pointer itself so
        // the target table can derive its key from the same runtime source.
        let frame_pointer = state[0];
        // Keep the gateway token at the same width as the VM state so a
        // 64-bit target authenticates the complete transition value.
        let gateway_scratch = builder.build_alloca(address_word, &value_name)?;
        let retry_budget = builder.build_alloca(word, &value_name)?;
        let scratch = builder.build_alloca(word, &value_name)?;
        // A continuation snapshots the lane before calling its helper and
        // verifies the helper's non-identity postcondition afterward. This
        // makes the semantic region a live part of the valid path.
        let helper_state_snapshot = builder.build_alloca(address_word, &value_name)?;
        let helper_guard = builder.build_alloca(address_word, &value_name)?;
        // Keep a sink family with all three marker encodings. Each
        // continuation selects one independently, so a function does not
        // expose one universal postcondition signature for all of its edges.
        for style in GuardStyle::ALL {
            let guard = GuardConfig::generate(&mut rng, address_word.get_bit_width(), style);
            let sink = guard.create_sink(module, address_word, &mut rng, &value_name)?;
            guard_sinks.push((sink, guard));
        }
        // Use a small family so valid continuations do not share one universal
        // call target. Each helper has the same local-only contract but a
        // different branch/arithmetic shape and constants.
        for _ in 0..2 {
            continuation_helpers.push(create_continuation_helper(module, address_word, &mut rng)?);
        }
        for slot in &state {
            builder
                .build_store(*slot, address_word.const_int(rng.random(), false))?
                .set_volatile(true)?;
        }
        builder
            .build_store(gateway_scratch, address_word.const_int(rng.random(), false))?
            .set_volatile(true)?;
        builder
            .build_store(retry_budget, word.const_int(rng.random_range(3..=9), false))?
            .set_volatile(true)?;
        let mut gateway_rows = Vec::with_capacity(transfers.len());
        let mut retry_rows = Vec::with_capacity(transfers.len());
        let mut reject_rows = Vec::with_capacity(transfers.len());
        let mut gateway_states = Vec::with_capacity(transfers.len());
        let mut dispatch_rows = Vec::with_capacity(transfers.len());
        let mut retry_arm_rows = Vec::with_capacity(transfers.len());
        for transfer in &transfers {
            let mut row = Vec::with_capacity(transfer.successors.len());
            let mut retries = Vec::with_capacity(transfer.successors.len());
            let mut rejects = Vec::with_capacity(transfer.successors.len());
            let mut continuations = Vec::with_capacity(transfer.successors.len());
            let mut dispatches = Vec::with_capacity(transfer.successors.len());
            let mut retry_arms = Vec::with_capacity(transfer.successors.len());
            let tags = Vec::with_capacity(transfer.successors.len());
            for _ in &transfer.successors {
                let gateway = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let retry = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let reject = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let continuation = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let dispatch = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let retry_arm = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                gateway_blocks.extend([gateway, retry, continuation, dispatch, retry_arm]);
                decoy_blocks.push(reject);
                row.push(gateway);
                retries.push(retry);
                rejects.push(reject);
                continuations.push(continuation);
                dispatches.push(dispatch);
                retry_arms.push(retry_arm);
            }
            gateway_rows.push(row);
            retry_rows.push(retries);
            reject_rows.push(rejects);
            continuation_rows.push(continuations);
            dispatch_rows.push(dispatches);
            retry_arm_rows.push(retry_arms);
            gateway_tags.push(tags);
        }
        let all_retries: Vec<_> = retry_rows.iter().flatten().copied().collect();
        ensure!(!all_retries.is_empty(), "VM gateway retry set is empty");
        let guard_assignments = GuardConfig::edge_assignments(&mut rng, all_retries.len());
        let mut gateway_index = 0usize;
        for (transfer_row, transfer) in transfers.iter().enumerate() {
            let mut incoming_states = Vec::with_capacity(transfer.successors.len());
            for (edge, target) in transfer.successors.iter().enumerate() {
                let gateway = gateway_rows[transfer_row][edge];
                let retry = retry_rows[transfer_row][edge];
                let reject = reject_rows[transfer_row][edge];
                let continuation = continuation_rows[transfer_row][edge];
                let dispatch = dispatch_rows[transfer_row][edge];
                let retry_arm = retry_arm_rows[transfer_row][edge];
                builder.position_at_end(gateway);
                // The target-table share family is packed into the high bit
                // of the row tag envelope. Keep the gateway payload in the
                // remaining bits so the lookup can recover the exact tag
                // before authenticating the continuation.
                let tag_mask = (1u64 << (address_word.get_bit_width() - 1)) - 1;
                let tag = rng.random::<u64>() & tag_mask;
                let codec_index = usize::try_from(tag & u64::from(TARGET_VARIANTS - 1))?;
                let codec = *transfer
                    .gateway_codecs
                    .get(codec_index)
                    .context("VM gateway codec family is missing")?;
                let loaded = builder
                    .build_load2(address_word, gateway_scratch, &value_name)?
                    .into_int_value();
                loaded
                    .as_instruction_value()
                    .context("gateway token is not a load")?
                    .set_volatile(true)?;
                let current_lanes = state
                    .iter()
                    .map(|slot| {
                        let loaded = builder.build_load2(address_word, *slot, &value_name)?.into_int_value();
                        loaded
                            .as_instruction_value()
                            .context("gateway state lane is not a load")?
                            .set_volatile(true)?;
                        Ok(loaded)
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                let current_state = state_digest(
                    &builder,
                    &current_lanes,
                    transfer.digest_salt,
                    transfer.digest_rotate,
                    &value_name,
                )?;
                let decoded = codec.decode(&builder, loaded, current_state, &value_name)?;
                incoming_states.push(current_state);
                let predicate = builder.build_int_compare(
                    IntPredicate::EQ,
                    decoded,
                    address_word.const_int(tag, false),
                    &value_name,
                )?;
                let choice_word = builder.build_and(current_state, address_word.const_int(1, false), &value_name)?;
                let choice =
                    builder.build_int_compare(IntPredicate::NE, choice_word, address_word.const_zero(), &value_name)?;
                builder.build_store(continuation_choice, choice)?.set_volatile(true)?;
                let mixed = builder.build_xor(loaded, address_word.const_int(rng.random(), false), &value_name)?;
                builder.build_store(gateway_scratch, mixed)?.set_volatile(true)?;
                builder.build_conditional_branch(predicate, continuation, retry)?;

                // Keep the valid path behind a private, non-inlined semantic
                // region. The helper mutates the state lane; the continuation
                // validates that mutation through the private sink before
                // becoming the PHI predecessor of the original target block.
                builder.position_at_end(continuation);
                let state_before = builder
                    .build_load2(address_word, state[0], &value_name)?
                    .into_int_value();
                state_before
                    .as_instruction_value()
                    .context("VM continuation state snapshot load")?
                    .set_volatile(true)?;
                builder
                    .build_store(helper_state_snapshot, state_before)?
                    .set_volatile(true)?;
                let helper = continuation_helpers[gateway_index % continuation_helpers.len()];
                builder.build_call(helper, &[gateway_scratch.into(), state[0].into()], &value_name)?;
                let state_after = builder
                    .build_load2(address_word, state[0], &value_name)?
                    .into_int_value();
                state_after
                    .as_instruction_value()
                    .context("VM continuation state postcondition load")?
                    .set_volatile(true)?;
                let state_before = builder
                    .build_load2(address_word, helper_state_snapshot, &value_name)?
                    .into_int_value();
                state_before
                    .as_instruction_value()
                    .context("VM continuation state snapshot reload")?
                    .set_volatile(true)?;
                let helper_changed =
                    builder.build_int_compare(IntPredicate::NE, state_after, state_before, &value_name)?;
                let sink_index = guard_assignments[gateway_index];
                let (sink, guard) = guard_sinks[sink_index];
                let helper_guard_value = guard.emit_marker(&builder, helper_changed, address_word, &value_name)?;
                builder
                    .build_store(helper_guard, helper_guard_value)?
                    .set_volatile(true)?;
                builder.build_call(sink, &[helper_guard.into()], &value_name)?;
                // Keep the semantic helper region separate from the target
                // dispatch. A dynamic retry arm can therefore return to the
                // dispatch without executing the state-mutating helper twice.
                builder.build_unconditional_branch(dispatch)?;
                continuation_edges.push((dispatch, *target, retry_arm, reject));

                // An invalid token does not expose a terminal rejection arm
                // directly. It enters a bounded, data-dependent retry graph
                // and eventually reaches the volatile sink. Retry fallbacks
                // never target real gateways, so they cannot add predecessors
                // to the PHI-bearing original blocks.
                builder.position_at_end(retry);
                let retry_token = builder
                    .build_load2(address_word, gateway_scratch, &value_name)?
                    .into_int_value();
                retry_token
                    .as_instruction_value()
                    .context("gateway retry token is not a load")?
                    .set_volatile(true)?;
                let retry_state = builder
                    .build_load2(address_word, state[0], &value_name)?
                    .into_int_value();
                retry_state
                    .as_instruction_value()
                    .context("gateway retry state is not a load")?
                    .set_volatile(true)?;
                let retry_mix = builder.build_xor(retry_token, retry_state, &value_name)?;
                let retry_entropy = builder.build_int_compare(
                    IntPredicate::EQ,
                    retry_mix,
                    address_word.const_int(rng.random(), false),
                    &value_name,
                )?;
                let budget = builder.build_load2(word, retry_budget, &value_name)?.into_int_value();
                budget
                    .as_instruction_value()
                    .context("gateway retry budget is not a load")?
                    .set_volatile(true)?;
                let budget_available =
                    builder.build_int_compare(IntPredicate::UGT, budget, word.const_zero(), &value_name)?;
                let next_budget = builder.build_int_sub(budget, word.const_int(1, false), &value_name)?;
                builder.build_store(retry_budget, next_budget)?.set_volatile(true)?;
                let can_retry = builder.build_and(budget_available, retry_entropy, &value_name)?;
                let fallback = if all_retries.len() == 1 {
                    all_retries[0]
                } else {
                    let offset = rng.random_range(1..all_retries.len());
                    all_retries[(gateway_index + offset) % all_retries.len()]
                };
                builder.build_conditional_branch(can_retry, fallback, reject)?;
                builder.position_at_end(reject);
                builder
                    .build_store(gateway_scratch, address_word.const_int(rng.random(), false))?
                    .set_volatile(true)?;
                builder.build_call(guard_sinks[0].0, &[gateway_scratch.into()], &value_name)?;
                // A failed authentication is semantically unreachable for a
                // valid VM transition. Keep the final recovery arm as a live,
                // volatile sink loop instead of emitting an explicit
                // `unreachable` terminator; this removes a cheap terminal-arm
                // signature while preserving fail-closed behavior.
                builder.build_unconditional_branch(reject)?;
                builder.position_at_end(retry_arm);
                builder
                    .build_store(continuation_choice, ctx.bool_type().const_zero())?
                    .set_volatile(true)?;
                builder.build_unconditional_branch(dispatch)?;
                gateway_tags[transfer_row].push(tag);
                gateway_index += 1;
            }
            gateway_states.push(incoming_states);
        }
        // Expand each logical edge into a small row-local family of physical
        // aliases.  The aliases point at the same gateway block, so this is a
        // target-record transformation only; it adds no branch edge or PHI
        // predecessor.  Source and decoder use the same state-derived alias
        // selector below, while state transitions continue to consume the
        // logical edge ordinal.
        let address_rows = gateway_rows
            .iter()
            .zip(&transfers)
            .map(|(row, transfer)| {
                let alias_count = usize::try_from(transfer.slot_aliases)?;
                row.iter()
                    .flat_map(|block| std::iter::repeat(*block).take(alias_count))
                    .map(|block| {
                        // SAFETY: Every gateway is a live block in this function.
                        unsafe { block.get_address() }.context("cannot take VM gateway address")
                    })
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        // Insert after the state allocas: querying the first insertion point
        // again would place table initialization before its frame dependency.
        builder.position_before(&entry.get_terminator().context("entry lost its terminator")?);
        let frame_nonce = builder.build_ptr_to_int(frame_pointer, address_word, &value_name)?;
        // Bind the edge-bytecode envelope to this invocation. The low 32 bits
        // are sufficient for IndexProgram's Z/(2^32) bijections; on 64-bit
        // targets the truncation deliberately avoids widening every VM
        // operation. This is an extraction-cost layer, not a secret key.
        let bytecode_nonce_mask = if address_word.get_bit_width() == word.get_bit_width() {
            frame_nonce
        } else {
            builder.build_int_truncate(frame_nonce, word, &value_name)?
        };
        let bytecode_nonce_mask =
            builder.build_xor(bytecode_nonce_mask, word.const_int(rng.random(), false), &value_name)?;
        let mut table = TargetTable::create(
            module,
            &builder,
            address_word,
            &address_rows,
            frame_nonce,
            frame_pointer,
            &mut rng,
        )?;
        for (transfer_row, (addresses, transfer)) in address_rows.iter().zip(&transfers).enumerate() {
            let alias_count = usize::try_from(transfer.slot_aliases)?;
            let tags = gateway_tags[transfer_row]
                .iter()
                .flat_map(|tag| std::iter::repeat(*tag).take(alias_count))
                .collect::<Vec<_>>();
            table.add_row(
                &builder,
                addresses,
                &tags,
                &transfer.address_programs,
                transfer.decode_mode,
                &mut rng,
            )?;
        }
        // Materialize original continuation targets once per unique block and
        // keep masked integers in a sparse invocation-local vault. Continuation
        // pods decode the two selected cells into runtime pointers instead of
        // embedding one pair of blockaddress conversions per edge. Retry arms
        // remain distinct targets, while duplicate original successors share a
        // cell. Slot zero and random holes keep this vault from looking like a
        // dense pointer table in the generated code.
        let mut continuation_addresses = Vec::new();
        let mut continuation_address_keys = HashSet::new();
        for (_, target, retry, _) in &continuation_edges {
            for block in [*target, *retry] {
                let address = unsafe { block.get_address() }.context("cannot take VM continuation address")?;
                let key = address.as_value_ref() as usize;
                if continuation_address_keys.insert(key) {
                    continuation_addresses.push((key, address));
                }
            }
        }
        ensure!(
            !continuation_addresses.is_empty(),
            "VM continuation address vault is empty"
        );
        let mut bucket_sources: HashMap<usize, (FunctionValue<'ctx>, u64, PointerValue<'ctx>, TargetResolverAbi)> =
            HashMap::new();
        let mut shuffled_addresses = continuation_addresses.clone();
        shuffled_addresses.shuffle(&mut rng);
        let mut bucket_cursor = 0usize;
        let mut resolver_index = 0usize;
        while bucket_cursor < shuffled_addresses.len() {
            let remaining = shuffled_addresses.len() - bucket_cursor;
            let bucket_len = if remaining == 1 {
                1
            } else if remaining == 3 {
                3
            } else if remaining >= 5 && rng.random_bool(0.5) {
                3
            } else if remaining >= 4 && rng.random_bool(0.5) {
                4
            } else {
                2
            };
            // Keep resolver buckets multi-target whenever the address set is
            // large enough.  In particular, avoid ending a five-entry tail
            // with a one-target helper after a random four-way choice.
            let bucket_len = if remaining > 1 && remaining - bucket_len == 1 {
                3
            } else {
                bucket_len
            };
            let take = remaining.min(bucket_len);
            let bucket_entries = &shuffled_addresses[bucket_cursor..bucket_cursor + take];
            let mut bucket_addresses: Vec<_> = bucket_entries.iter().map(|(_, address)| *address).collect();
            while bucket_addresses.len() < bucket_len {
                let filler = *bucket_addresses
                    .first()
                    .context("VM target resolver bucket has no filler address")?;
                bucket_addresses.push(filler);
            }
            let abi = if resolver_index.is_multiple_of(2) {
                TargetResolverAbi::NonceFrameSelectorBucket
            } else {
                TargetResolverAbi::FrameNonceBucketSelector
            };
            let (helper, selector_tokens) =
                table.create_target_bucket_materializer(module, bucket_addresses.len(), abi, &mut rng, &value_name)?;
            let bucket =
                table.create_target_bucket_vault(module, &builder, &bucket_addresses, &mut rng, &value_name)?;
            for (selector, (key, _)) in bucket_entries.iter().enumerate() {
                bucket_sources.insert(
                    *key,
                    (
                        helper,
                        *selector_tokens
                            .get(selector)
                            .context("VM continuation resolver selector is missing")?,
                        bucket,
                        abi,
                    ),
                );
            }
            bucket_cursor += take;
            resolver_index += 1;
        }
        let address_count = u32::try_from(continuation_addresses.len())?;
        let hole_count = rng.random_range(1..=address_count.min(8));
        let vault_len = address_count
            .checked_add(hole_count)
            .and_then(|length| length.checked_add(1))
            .context("VM continuation address vault is too large")?;
        let vault_type = address_word.array_type(vault_len);
        let vault = builder.build_alloca(vault_type, &value_name)?;
        let selector_vault = builder.build_alloca(vault_type, &value_name)?;
        let selector_mask = builder.build_xor(frame_nonce, address_word.const_int(rng.random(), false), &value_name)?;
        let mut vault_slots: Vec<u32> = (1..vault_len).collect();
        vault_slots.shuffle(&mut rng);
        let mut continuation_address_entries: HashMap<usize, (u32, u64)> = HashMap::new();
        builder.position_before(&entry.get_terminator().context("entry lost its terminator")?);
        let header = builder.build_in_bounds_gep2(
            vault_type,
            vault,
            &[address_word.const_zero(), address_word.const_zero()],
            &value_name,
        )?;
        builder
            .build_store(header, address_word.const_int(rng.random(), false))?
            .set_volatile(true)?;
        for slot in 1..vault_len {
            let cell = builder.build_in_bounds_gep2(
                vault_type,
                vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(slot), false),
                ],
                &value_name,
            )?;
            builder
                .build_store(cell, address_word.const_int(rng.random(), false))?
                .set_volatile(true)?;
            let selector_cell = builder.build_in_bounds_gep2(
                vault_type,
                selector_vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(slot), false),
                ],
                &value_name,
            )?;
            builder
                .build_store(selector_cell, address_word.const_int(rng.random(), false))?
                .set_volatile(true)?;
        }
        for (index, (key, _address)) in continuation_addresses.iter().enumerate() {
            let slot = vault_slots[index];
            let salt = rng.random::<u64>();
            let (helper, selector, bucket, abi) = *bucket_sources
                .get(key)
                .context("VM continuation target resolver is missing")?;
            let masked_selector =
                builder.build_xor(selector_mask, address_word.const_int(selector, false), &value_name)?;
            let selector_cell = builder.build_in_bounds_gep2(
                vault_type,
                selector_vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(slot), false),
                ],
                &value_name,
            )?;
            builder
                .build_store(selector_cell, masked_selector)?
                .set_volatile(true)?;
            let loaded_selector = builder
                .build_load2(address_word, selector_cell, &value_name)?
                .into_int_value();
            loaded_selector
                .as_instruction_value()
                .context("VM continuation selector vault load")?
                .set_volatile(true)?;
            let selector = builder.build_xor(loaded_selector, selector_mask, &value_name)?;
            let encoded_selector = builder.build_xor(frame_nonce, selector, &value_name)?;
            let materialized =
                table.call_target_bucket_materializer(&builder, helper, encoded_selector, bucket, abi, &value_name)?;
            let mask = builder.build_xor(frame_nonce, address_word.const_int(salt, false), &value_name)?;
            let encoded = builder.build_xor(materialized, mask, &value_name)?;
            let cell = builder.build_in_bounds_gep2(
                vault_type,
                vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(slot), false),
                ],
                &value_name,
            )?;
            builder.build_store(cell, encoded)?.set_volatile(true)?;
            continuation_address_entries.insert(*key, (slot, salt));
        }
        // Keep blockaddress constants in private frame-bound helpers while
        // giving each continuation a volatile runtime choice between its
        // real target and retry corridor. That unknown choice prevents O2/O3
        // from proving the indirectbr target is one fixed block. The helper
        // itself receives decoded runtime pointers, so it contains no direct
        // blockaddress expression.
        for (dispatch, target, retry, reject) in continuation_edges {
            builder.position_at_end(dispatch);
            let target_address = unsafe { target.get_address() }.context("cannot take VM continuation address")?;
            let retry_address = unsafe { retry.get_address() }.context("cannot take VM retry address")?;
            let (target_slot, target_salt) = *continuation_address_entries
                .get(&(target_address.as_value_ref() as usize))
                .context("VM continuation target vault is missing")?;
            let (retry_slot, retry_salt) = *continuation_address_entries
                .get(&(retry_address.as_value_ref() as usize))
                .context("VM continuation retry vault is missing")?;
            let target_cell = builder.build_in_bounds_gep2(
                vault_type,
                vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(target_slot), false),
                ],
                &value_name,
            )?;
            let encoded_target = builder
                .build_load2(address_word, target_cell, &value_name)?
                .into_int_value();
            encoded_target
                .as_instruction_value()
                .context("VM continuation target vault load has no instruction")?
                .set_volatile(true)?;
            let target_mask =
                builder.build_xor(frame_nonce, address_word.const_int(target_salt, false), &value_name)?;
            let target_value = builder.build_xor(encoded_target, target_mask, &value_name)?;
            let target_pointer = builder.build_int_to_ptr(target_value, target_address.get_type(), &value_name)?;
            let retry_cell = builder.build_in_bounds_gep2(
                vault_type,
                vault,
                &[
                    address_word.const_zero(),
                    address_word.const_int(u64::from(retry_slot), false),
                ],
                &value_name,
            )?;
            let encoded_retry = builder
                .build_load2(address_word, retry_cell, &value_name)?
                .into_int_value();
            encoded_retry
                .as_instruction_value()
                .context("VM continuation retry vault load has no instruction")?
                .set_volatile(true)?;
            let retry_mask = builder.build_xor(frame_nonce, address_word.const_int(retry_salt, false), &value_name)?;
            let retry_value = builder.build_xor(encoded_retry, retry_mask, &value_name)?;
            let retry_pointer = builder.build_int_to_ptr(retry_value, retry_address.get_type(), &value_name)?;
            let helper = table.create_continuation_materializer(
                module,
                target_address.get_type(),
                continuation_choice,
                &mut rng,
                &value_name,
            )?;
            let encoded = table.call_continuation_materializer(
                &builder,
                helper,
                continuation_choice,
                target_pointer,
                retry_pointer,
                &value_name,
            )?;
            let address = target_address;
            let decoded = builder.build_int_to_ptr(encoded, address.get_type(), &value_name)?;
            builder.build_indirect_branch(decoded, &[target, retry, reject])?;
        }
        // Keep every indirect-branch destination set from being an exact copy
        // of the original successor set. These islands are never encoded in
        // the target pool; they are valid LLVM destinations whose internal
        // arithmetic/branch shape makes the extra labels less trivial to
        // discard than a bare `unreachable` block. Real targets are gateways,
        // and their PHI predecessors are repaired below. Each transfer row
        // receives an independent island family so all dispatchers do not
        // share a common decoy-label intersection.
        for _ in 0..transfers.len() {
            let mut row_targets = Vec::new();
            for _ in 0..rng.random_range(1..=3) {
                let island = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let taken = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                let fallthrough = ctx.append_basic_block(function, &format!("v{:016x}", rng.random::<u64>()));
                row_targets.push(island);
                decoy_blocks.extend([island, taken, fallthrough]);

                builder.position_at_end(island);
                let slot = builder.build_alloca(word, &value_name)?;
                let seed = word.const_int(rng.random(), false);
                let mixed = builder.build_xor(seed, word.const_int(rng.random(), false), &value_name)?;
                builder.build_store(slot, mixed)?.set_volatile(true)?;
                let loaded = builder.build_load2(word, slot, &value_name)?.into_int_value();
                let condition = builder.build_int_compare(IntPredicate::EQ, loaded, word.const_zero(), &value_name)?;
                builder.build_conditional_branch(condition, taken, fallthrough)?;

                for block in [taken, fallthrough] {
                    builder.position_at_end(block);
                    let value = builder.build_int_add(
                        word.const_int(rng.random(), false),
                        word.const_int(rng.random(), false),
                        &value_name,
                    )?;
                    builder.build_store(slot, value)?.set_volatile(true)?;
                    builder.build_unreachable()?;
                }
            }
            decoy_targets.push(row_targets);
        }
        for (transfer_row, transfer) in transfers.iter().enumerate() {
            builder.position_before(&transfer.terminator);
            let current_lanes = state
                .iter()
                .map(|slot| {
                    let loaded = builder.build_load2(address_word, *slot, &value_name)?.into_int_value();
                    loaded
                        .as_instruction_value()
                        .context("VM state lane is not a load")?
                        .set_volatile(true)?;
                    Ok(loaded)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let current_state = state_digest(
                &builder,
                &current_lanes,
                transfer.digest_salt,
                transfer.digest_rotate,
                &value_name,
            )?;
            let bytecode_state_mask = if address_word.get_bit_width() == word.get_bit_width() {
                current_state
            } else {
                builder.build_int_truncate(current_state, word, &value_name)?
            };
            let bytecode_mask = builder.build_xor(bytecode_nonce_mask, bytecode_state_mask, &value_name)?;
            // Key arithmetic constants with a transfer-local, envelope-shaped
            // schedule shared by the source inverse and decoder forward
            // programs. The schedule is distinct from the edge bytecode
            // envelope, so the two dataflows do not collapse to one decoder
            // fingerprint.
            let opcode_key =
                transfer
                    .opcode_schedule
                    .encode(&builder, bytecode_mask, bytecode_state_mask, &value_name)?;
            // IndexProgram derives a selector schedule from this key and the
            // original operation positions. Both directions materialize the
            // same recurrence before walking their respective operation order,
            // so each step can choose a different candidate without breaking
            // the inverse relationship.
            // Select the plain successor ordinal first. The inverse program
            // is emitted on this runtime path, so no static pre-encoded
            // successor constants disclose the row's index mapping.
            let ordinals: Vec<_> = transfer
                .successors
                .iter()
                .enumerate()
                .map(|(index, _)| Ok(word.const_int(u64::try_from(index)?, false)))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let logical_input = select_input(&builder, transfer, &ordinals, &value_name)?;
            let alias = builder.build_int_unsigned_rem(
                bytecode_state_mask,
                word.const_int(u64::from(transfer.slot_aliases), false),
                &value_name,
            )?;
            let physical_input = builder.build_int_add(
                builder.build_int_mul(
                    logical_input,
                    word.const_int(u64::from(transfer.slot_aliases), false),
                    &value_name,
                )?,
                alias,
                &value_name,
            )?;
            let input = IndexProgram::emit_inverse_variants(
                &transfer.programs,
                &builder,
                physical_input,
                &transfer.substitutions,
                opcode_key,
            )?;
            let input = transfer
                .bytecode_envelope
                .encode(&builder, input, bytecode_state_mask, &value_name)?;
            let input = builder.build_xor(input, bytecode_mask, &value_name)?;
            let input = builder.build_xor(
                input,
                word.const_int(u64::from(transfer.bytecode_salt), false),
                &value_name,
            )?;
            // A local slot avoids shared mutable state between threads and
            // recursive invocations. Volatile prevents constant folding of the
            // input; it is an optimization barrier, not a secrecy guarantee.
            builder.build_store(scratch, input)?.set_volatile(true)?;
            let loaded = builder.build_load2(word, scratch, &value_name)?;
            loaded
                .as_instruction_value()
                .context("VM input is not a load")?
                .set_volatile(true)?;
            let loaded = loaded.into_int_value();
            let unmasked = builder.build_xor(loaded, bytecode_mask, &value_name)?;
            let unmasked = builder.build_xor(
                unmasked,
                word.const_int(u64::from(transfer.bytecode_salt), false),
                &value_name,
            )?;
            let unmasked = transfer
                .bytecode_envelope
                .decode(&builder, unmasked, bytecode_state_mask, &value_name)?;
            let physical_index = IndexProgram::emit_variants(
                &transfer.programs,
                &builder,
                unmasked,
                &transfer.substitutions,
                opcode_key,
            )?;
            let index = if transfer.slot_aliases == 1 {
                physical_index
            } else {
                builder.build_int_unsigned_div(
                    physical_index,
                    word.const_int(u64::from(transfer.slot_aliases), false),
                    &value_name,
                )?
            };
            let witnesses = transfer
                .witnesses
                .iter()
                .map(|witness| freeze_witness(&builder, *witness, &value_name))
                .collect::<Vec<_>>();
            let next_lanes = transfer
                .state_programs
                .iter()
                .zip(&current_lanes)
                .map(|(program, current)| program.update(&builder, *current, index, &witnesses, frame_nonce))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let next_state = state_digest(
                &builder,
                &next_lanes,
                transfer.digest_salt,
                transfer.digest_rotate,
                &value_name,
            )?;
            let target_state = match transfer.state_timing {
                StateTiming::BeforeTarget => next_state,
                StateTiming::AfterTarget => current_state,
            };
            let store_before_target = matches!(transfer.state_timing, StateTiming::BeforeTarget);
            if store_before_target {
                for (slot, lane) in state.iter().zip(&next_lanes) {
                    builder.build_store(*slot, *lane)?.set_volatile(true)?;
                }
            }
            let lookup = table.lookup(&builder, u32::try_from(transfer_row)?, physical_index, target_state)?;
            let target = lookup.pointer;
            if !store_before_target {
                for (slot, lane) in state.iter().zip(&next_lanes) {
                    builder.build_store(*slot, *lane)?.set_volatile(true)?;
                }
            }
            // The gateway authenticates the row-local edge tag loaded by the
            // same lookup that reconstructed the target. This creates a live
            // cross-block dependency; it is not an identity predicate over
            // arbitrary input and cannot be folded without tracing the VM
            // transition and its record layout.
            let mut token = None;
            let codec_selector = builder.build_and(
                lookup.gateway_tag,
                address_word.const_int(u64::from(TARGET_VARIANTS - 1), false),
                &value_name,
            )?;
            for (codec_index, codec) in transfer.gateway_codecs.iter().copied().enumerate() {
                let candidate = codec.encode(&builder, next_state, lookup.gateway_tag, &value_name)?;
                token = Some(match token {
                    None => candidate,
                    Some(selected) => {
                        let matches = builder.build_int_compare(
                            IntPredicate::EQ,
                            codec_selector,
                            address_word.const_int(u64::try_from(codec_index)?, false),
                            &value_name,
                        )?;
                        builder
                            .build_select(matches, candidate, selected, &value_name)?
                            .into_int_value()
                    },
                });
            }
            let token = token.context("VM gateway codecs are empty")?;
            builder.build_store(gateway_scratch, token)?.set_volatile(true)?;
            // Transport real PHI values through the VM state. Encoders stay
            // on the original predecessor; decoders use the gateway's state
            // loads and remain on the corresponding edge. Capture all inputs
            // before rewriting so loop-carried PHIs retain parallel semantics.
            for (edge, inputs) in transfer.phi_inputs.iter().enumerate() {
                let gateway = gateway_rows[transfer_row][edge];
                for input in inputs {
                    builder.position_before(&transfer.terminator);
                    let codec = PhiCodec::generate(&mut rng);
                    let encoded = codec.encode(&builder, input.value, next_state, &value_name)?;
                    builder.position_before(&gateway.get_terminator().context("VM gateway has no terminator")?);
                    let decoded = codec.decode(&builder, encoded, gateway_states[transfer_row][edge], &value_name)?;
                    phi_values.push(PhiRewrite::new(*input, decoded));
                }
            }
            builder.position_before(&transfer.terminator);
            let mut destinations = gateway_rows[transfer_row].clone();
            destinations.extend(decoy_targets[transfer_row].iter().copied());
            // The order of an LLVM indirectbr destination list is not
            // semantic, but retaining logical successor order gives a static
            // extractor a cheap alignment signal between the source edge
            // ordinal and the legal target set.  The target table already
            // uses independent row-local permutations; apply the same idea
            // to the legal destination list so its textual order carries no
            // edge meaning.
            if destinations.len() > 1 {
                let logical_order = destinations.clone();
                destinations.shuffle(&mut rng);
                // A seeded shuffle can legally return the identity
                // permutation.  Force a non-identity order in that case so a
                // row never accidentally retains the source-edge alignment
                // this layer is intended to remove.
                if destinations == logical_order {
                    destinations.rotate_left(1);
                }
            }
            builder.build_indirect_branch(target, &destinations)?;
            transfer.terminator.remove_from_basic_block();
            // Detached terminators still count as users of their conditions
            // and successor blocks. Temporarily unlink those uses as well;
            // otherwise the verifier sees spurious predecessors and detached
            // instruction users. Save raw operands, including BasicBlocks.
            let operands: Vec<_> = (0..transfer.terminator.get_num_operands())
                .map(|index| {
                    // SAFETY: The detached instruction is live, the operand index
                    // is in range, and every referenced original value stays live.
                    // LLVM User permits null operands while an IR node is detached.
                    unsafe {
                        let value = LLVMGetOperand(transfer.terminator.as_value_ref(), index);
                        LLVMSetOperand(transfer.terminator.as_value_ref(), index, std::ptr::null_mut());
                        value
                    }
                })
                .collect();
            detached.push((transfer.block, transfer.terminator, operands));
        }
        // Dispatch blocks are now the only predecessors reaching the original target
        // blocks. Replace each original predecessor in every target PHI, while
        // preserving duplicate incoming entries for duplicate switch edges.
        for (transfer, continuations) in transfers.iter().zip(&dispatch_rows) {
            let mut grouped = Vec::<(BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>)>::new();
            for (target, continuation) in transfer.successors.iter().zip(continuations) {
                if let Some((_, candidates)) = grouped.iter_mut().find(|(candidate, _)| candidate == target) {
                    candidates.push(*continuation);
                } else {
                    grouped.push((*target, vec![*continuation]));
                }
            }
            for (target, new_preds) in grouped {
                target.fix_phi_node_edges(transfer.block, &new_preds);
                phi_rewrites.push((target, transfer.block, new_preds));
            }
        }
        for rewrite in &phi_values {
            rewrite.apply()?;
        }
        if let VerifyResult::Broken(message) = function.verify_function() {
            anyhow::bail!("distributed VM verification failed: {message}");
        }
        Ok(())
    })();

    if let Err(error) = result {
        for rewrite in &phi_values {
            rewrite.restore();
        }
        for (target, old, new_preds) in phi_rewrites.iter().rev() {
            for new in new_preds {
                target.fix_phi_node(*new, *old);
            }
        }
        // Remove gateway decoders before their predecessor encoders and state
        // allocas. PHI uses have already been restored above. Keep the blocks
        // alive until the original-block dispatch branches are removed.
        let mut generated_blocks = decoy_blocks;
        generated_blocks.extend(gateway_blocks);
        for block in &generated_blocks {
            let instructions: Vec<_> = block.get_instructions().collect();
            for instruction in instructions.into_iter().rev() {
                instruction.erase_from_basic_block();
            }
        }
        for block in blocks.iter().rev() {
            let added: Vec<_> = block
                .get_instructions()
                .filter(|inst| !original.contains(inst))
                .collect();
            for instruction in added.into_iter().rev() {
                instruction.erase_from_basic_block();
            }
        }
        for (block, terminator, operands) in detached {
            for (index, operand) in operands.into_iter().enumerate() {
                // SAFETY: Restore the exact original operands in their original
                // slots before embedding the instruction in the function again.
                unsafe { LLVMSetOperand(terminator.as_value_ref(), index as u32, operand) };
            }
            builder.position_at_end(block);
            builder.insert_instruction(&terminator, None);
        }
        // Generated gateway/retry/rejection blocks can reference one another.
        // Erase their terminators and instructions before deleting any block;
        // deleting one live block first would leave a dangling block operand
        // in another generated terminator (LLVM may crash while destroying the
        // resulting ConstantPointerNull).
        for block in generated_blocks {
            // SAFETY: All references from original and generated instructions
            // were removed above, and PHI predecessors were restored earlier.
            unsafe { LLVMDeleteBasicBlock(block.as_mut_ptr()) };
        }
        for (sink, _) in guard_sinks.drain(..) {
            // SAFETY: Calls to the sinks were removed with generated blocks,
            // and these private helpers have no remaining users in the
            // restored function.
            unsafe { sink.delete() };
        }
        for helper in continuation_helpers {
            // SAFETY: Continuation calls were removed with generated blocks,
            // and these private helpers have no remaining users after rollback.
            unsafe { helper.delete() };
        }
        return Err(error);
    }
    for (block, terminator, _) in detached {
        builder.position_before(&block.get_terminator().context("missing VM indirectbr")?);
        builder.insert_instruction(&terminator, None);
        terminator.erase_from_basic_block();
    }
    function.clear_stale_analysis_attrs_after_cfg_rewrite();
    let done = done_attribute_name(function);
    function.add_attribute(AttributeLoc::Function, ctx.create_string_attribute(&done, "1"));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{BytecodeEnvelope, DecodeMode, GatewayCodec, GuardConfig, GuardStyle, StateTiming};
    use amice_plugin::inkwell::{OptimizationLevel, context::Context};
    use rand::{Rng, SeedableRng, rngs::StdRng};
    use std::collections::HashSet;

    #[test]
    fn state_timing_generation_covers_both_lowerings() {
        let mut seen_before = false;
        let mut seen_after = false;
        for seed in [0, 1, 42, u64::MAX] {
            let mut rng = StdRng::seed_from_u64(seed);
            match StateTiming::generate(&mut rng) {
                StateTiming::BeforeTarget => seen_before = true,
                StateTiming::AfterTarget => seen_after = true,
            }
        }
        assert!(seen_before && seen_after);
    }

    #[test]
    fn decoder_modes_are_distinct_and_cover_both_envelopes() {
        assert_ne!(DecodeMode::CandidateSelect, DecodeMode::SharedProgram);
        let mut seen_candidate = false;
        let mut seen_shared = false;
        for seed in [0, 1, 42, u64::MAX] {
            let mut rng = StdRng::seed_from_u64(seed);
            let mode = if rng.random_bool(0.5) {
                DecodeMode::CandidateSelect
            } else {
                DecodeMode::SharedProgram
            };
            match mode {
                DecodeMode::CandidateSelect => seen_candidate = true,
                DecodeMode::SharedProgram => seen_shared = true,
            }
        }
        assert!(seen_candidate && seen_shared);
    }

    #[test]
    fn bytecode_envelopes_round_trip_state_selected_values() {
        let mut seen_affine = false;
        let mut seen_rotate = false;
        let mut seen_add_xor = false;
        for seed in 0..512 {
            let envelope = BytecodeEnvelope::generate(&mut StdRng::seed_from_u64(seed));
            let seed = u32::try_from(seed).expect("test seed fits in u32");
            match envelope {
                BytecodeEnvelope::Affine { .. } => seen_affine = true,
                BytecodeEnvelope::RotateXor { .. } => seen_rotate = true,
                BytecodeEnvelope::AddXor { .. } => seen_add_xor = true,
            }
            for value in [0, 1, 31, 0xff, 0x8000_0000, u32::MAX, seed, !seed] {
                for state in [0, 1, 3, 0x1234_5678, 0x8000_0000, u32::MAX] {
                    assert_eq!(
                        envelope.host_decode(envelope.host_encode(value, state), state),
                        value,
                        "seed={seed}, value={value}, state={state}"
                    );
                }
            }
        }
        assert!(seen_affine && seen_rotate && seen_add_xor);
    }

    #[test]
    fn guard_markers_keep_success_and_reject_states_separate() {
        for bits in [32, 64] {
            let mask = if bits == 32 { u64::from(u32::MAX) } else { u64::MAX };
            for seed in 0..128 {
                let mut rng = StdRng::seed_from_u64(seed);
                for style in GuardStyle::ALL {
                    let guard = GuardConfig::generate(&mut rng, bits, style);
                    assert_ne!(guard.accept, guard.reject);
                    assert_eq!(guard.accept & mask, guard.accept);
                    assert_eq!(guard.reject & mask, guard.reject);
                    assert_ne!(guard.reject_mix & mask, 0);
                    assert_ne!((guard.reject ^ guard.reject_mix) & mask, guard.accept);
                }
            }
        }
    }

    #[test]
    fn guard_edge_assignments_cover_the_available_families_reproducibly() {
        for seed in [0, 1, 42, u64::MAX] {
            for edges in [1, 2, 3, 4, 64] {
                let first = GuardConfig::edge_assignments(&mut StdRng::seed_from_u64(seed), edges);
                let second = GuardConfig::edge_assignments(&mut StdRng::seed_from_u64(seed), edges);
                assert_eq!(first, second);
                assert_eq!(first.len(), edges);
                assert!(first.iter().all(|index| *index < GuardStyle::ALL.len()));
                assert_eq!(
                    first.iter().copied().collect::<HashSet<_>>().len(),
                    edges.min(GuardStyle::ALL.len())
                );
            }
        }
    }

    #[test]
    fn gateway_codecs_cover_families_and_round_trip() {
        let mut rng = StdRng::seed_from_u64(0);
        for bits in [32, 64] {
            let context = Context::create();
            let abi_word = context.i64_type();
            let word = if bits == 32 { context.i32_type() } else { abi_word };
            let codecs = [
                GatewayCodec::Xor,
                GatewayCodec::Add,
                GatewayCodec::Sub,
                GatewayCodec::RotatedXor { shift: 1 },
                GatewayCodec::RotatedXor { shift: bits - 1 },
            ];
            for codec in codecs {
                let module = context.create_module("gateway_codec");
                let builder = context.create_builder();
                let function = module.add_function(
                    "round_trip",
                    abi_word.fn_type(&[abi_word.into(), abi_word.into()], false),
                    None,
                );
                builder.position_at_end(context.append_basic_block(function, "entry"));
                let state = function.get_first_param().unwrap().into_int_value();
                let tag = function.get_nth_param(1).unwrap().into_int_value();
                let (state, tag) = if bits == 32 {
                    (
                        builder.build_int_truncate(state, word, "state").unwrap(),
                        builder.build_int_truncate(tag, word, "tag").unwrap(),
                    )
                } else {
                    (state, tag)
                };
                let token = codec.encode(&builder, state, tag, "token").unwrap();
                let decoded = codec.decode(&builder, token, state, "decoded").unwrap();
                let result = if bits == 32 {
                    builder.build_int_z_extend(decoded, abi_word, "result").unwrap()
                } else {
                    decoded
                };
                builder.build_return(Some(&result)).unwrap();
                module.verify().unwrap();
                let engine = module.create_jit_execution_engine(OptimizationLevel::None).unwrap();
                // SAFETY: the verified function has this C ABI, and the engine
                // and its module remain alive for every call below.
                let round_trip =
                    unsafe { engine.get_function::<unsafe extern "C" fn(u64, u64) -> u64>("round_trip") }.unwrap();
                let boundaries = [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 1 << 63, u64::MAX];
                let cases = boundaries
                    .into_iter()
                    .flat_map(|state| boundaries.into_iter().map(move |tag| (state, tag)));
                let mask = if bits == 32 { u64::from(u32::MAX) } else { u64::MAX };
                for (state, tag) in cases.chain((0..128).map(|_| (rng.random(), rng.random()))) {
                    // SAFETY: integer arguments satisfy the verified signature.
                    let actual = unsafe { round_trip.call(state, tag) };
                    assert_eq!(actual, tag & mask, "bits={bits}, state={state}, tag={tag}");
                }
            }
        }

        let mut seen = HashSet::new();
        for _ in 0..128 {
            seen.insert(GatewayCodec::generate(&mut rng, 64));
        }
        assert!(seen.contains(&GatewayCodec::Xor));
        assert!(seen.contains(&GatewayCodec::Add));
        assert!(seen.contains(&GatewayCodec::Sub));
        assert!(
            seen.iter()
                .any(|codec| matches!(codec, GatewayCodec::RotatedXor { .. }))
        );
    }
}

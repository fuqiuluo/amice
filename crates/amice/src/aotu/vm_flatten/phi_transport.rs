use amice_plugin::inkwell::{
    basic_block::BasicBlock,
    builder::Builder,
    values::{InstructionOpcode, InstructionValue, IntValue, PhiValue},
};
use anyhow::{Context, anyhow, ensure};
use rand::Rng;

/// Original incoming values are captured before any CFG or PHI mutation.
#[derive(Clone, Copy)]
pub(super) struct PhiInput<'ctx> {
    phi: InstructionValue<'ctx>,
    index: u32,
    target: BasicBlock<'ctx>,
    predecessor: BasicBlock<'ctx>,
    occurrence: usize,
    pub(super) value: IntValue<'ctx>,
}

impl<'ctx> PhiInput<'ctx> {
    pub(super) fn on_edge(
        target: BasicBlock<'ctx>,
        predecessor: BasicBlock<'ctx>,
        occurrence: usize,
    ) -> anyhow::Result<Vec<Self>> {
        let mut inputs = Vec::new();
        for instruction in target
            .get_instructions()
            .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        {
            let phi = PhiValue::try_from(instruction).map_err(|_| anyhow!("instruction has PHI opcode"))?;
            let (index, (value, _)) = phi
                .get_incomings()
                .enumerate()
                .filter(|(_, (_, block))| *block == predecessor)
                .nth(occurrence)
                .context("VM PHI is missing an incoming edge occurrence")?;
            // Pointer, vector, floating-point and aggregate PHIs keep their
            // original values; only their predecessor blocks are repaired.
            if value.is_int_value() {
                inputs.push(Self {
                    phi: instruction,
                    // LLVM's PHI C API indexes incoming values directly;
                    // incoming blocks are queried through LLVMGetIncomingBlock
                    // rather than occupying alternating User operands.
                    index: u32::try_from(index)?,
                    target,
                    predecessor,
                    occurrence,
                    value: value.into_int_value(),
                });
            }
        }
        Ok(inputs)
    }
}

#[derive(Clone, Copy)]
enum TransportOp {
    Xor,
    Add,
    ReverseSub,
}

pub(super) struct PhiCodec {
    operation: TransportOp,
    salt: u64,
}

impl PhiCodec {
    pub(super) fn generate(rng: &mut impl Rng) -> Self {
        Self {
            operation: match rng.random_range(0..3) {
                0 => TransportOp::Xor,
                1 => TransportOp::Add,
                _ => TransportOp::ReverseSub,
            },
            salt: rng.random(),
        }
    }

    fn key<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let state = match state.get_type().get_bit_width().cmp(&word.get_bit_width()) {
            std::cmp::Ordering::Less => builder.build_int_z_extend(state, word, name)?,
            std::cmp::Ordering::Equal => state,
            std::cmp::Ordering::Greater => builder.build_int_truncate(state, word, name)?,
        };
        Ok(builder.build_xor(state, word.const_int(self.salt, false), name)?)
    }

    pub(super) fn encode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let key = self.key(builder, value, state, name)?;
        // Wrapping integer operations preserve the incoming value's poison
        // behavior. It never feeds VM control and is not stored to memory.
        Ok(match self.operation {
            TransportOp::Xor => builder.build_xor(value, key, name)?,
            TransportOp::Add => builder.build_int_add(value, key, name)?,
            TransportOp::ReverseSub => builder.build_int_sub(key, value, name)?,
        })
    }

    pub(super) fn decode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        state: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let key = self.key(builder, value, state, name)?;
        Ok(match self.operation {
            TransportOp::Xor => builder.build_xor(value, key, name)?,
            TransportOp::Add => builder.build_int_sub(value, key, name)?,
            TransportOp::ReverseSub => builder.build_int_sub(key, value, name)?,
        })
    }
}

pub(super) struct PhiRewrite<'ctx> {
    input: PhiInput<'ctx>,
    decoded: IntValue<'ctx>,
}

impl<'ctx> PhiRewrite<'ctx> {
    pub(super) fn new(input: PhiInput<'ctx>, decoded: IntValue<'ctx>) -> Self {
        Self { input, decoded }
    }

    pub(super) fn apply(&self) -> anyhow::Result<()> {
        ensure!(
            self.input.phi.set_operand(self.input.index, self.decoded),
            "VM PHI incoming index changed during edge repair: target %{}, predecessor %{}, occurrence {}, raw index {}, operand count {}",
            self.input.target.get_name().to_string_lossy(),
            self.input.predecessor.get_name().to_string_lossy(),
            self.input.occurrence,
            self.input.index,
            self.input.phi.get_num_operands()
        );
        Ok(())
    }

    pub(super) fn restore(&self) {
        // Edge repair replaces blocks in place, preserving the operand index.
        self.input.phi.set_operand(self.input.index, self.input.value);
    }
}

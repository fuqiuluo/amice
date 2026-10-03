use super::done_attribute_name;
use amice_llvm::inkwell2::BuilderExt;
use amice_plugin::inkwell::{
    AddressSpace, IntPredicate,
    attributes::{Attribute, AttributeLoc},
    builder::Builder,
    module::{Linkage, Module},
    targets::TargetData,
    types::{ArrayType, IntType, PointerType},
    values::{AsValueRef, BasicValue, FunctionValue, IntValue, PointerValue},
};
use anyhow::{Context, ensure};
use rand::{Rng, seq::SliceRandom};
use std::collections::{HashMap, HashSet};

/// A small bounded state domain keeps the generated table practical while
/// making the selected decoder depend on the path that reached a transfer.
pub(super) const TARGET_VARIANTS: u32 = 4;

#[derive(Clone, Copy)]
enum AddressOp {
    Add(u64),
    Sub(u64),
    Xor(u64),
    Multiply(u64),
    Rotate(u32),
    XorShiftRight(u32),
}

#[derive(Clone, Copy)]
enum StaticOp {
    Add(u64),
}

/// A second, constant-expression-only envelope for a pool index.
///
/// LLVM versions supported by the plugin do not expose every integer
/// constant-expression operation uniformly, so this deliberately uses only
/// add. The runtime removes this envelope before applying the
/// invocation-local address codec. The envelope is materialized in the local
/// record, so no static array of edge records is emitted.
struct StaticProgram {
    operations: Vec<StaticOp>,
}

impl StaticProgram {
    fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        // Keep one static expression operation. Multiple nested symbolic
        // additions are not representable by every 32-bit backend, while
        // this single envelope is supported by all LLVM targets here.
        let count = 1;
        let operations = (0..count).map(|_| StaticOp::Add(Self::constant(rng, bits))).collect();
        Self { operations }
    }

    fn constant(rng: &mut impl Rng, bits: u32) -> u64 {
        if bits == 32 {
            rng.random_range(1..=0x1_0000_u64)
        } else {
            rng.random()
        }
    }

    fn encode_constant<'ctx>(&self, mut value: IntValue<'ctx>) -> IntValue<'ctx> {
        let word = value.get_type();
        for operation in &self.operations {
            let constant = |key| word.const_int(key, false);
            value = match *operation {
                StaticOp::Add(key) => value.const_add(constant(key)),
            };
        }
        value
    }

    fn decode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        mut value: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        for operation in self.operations.iter().rev() {
            let constant = |key| word.const_int(key, false);
            value = match *operation {
                StaticOp::Add(key) => builder.build_int_sub(value, constant(key), name)?,
            };
        }
        Ok(value)
    }
}

/// A branch-free, bijective address codec. Each transfer owns a different
/// program; no common decoder shape is emitted for the whole function.
#[derive(Clone)]
pub(super) struct AddressProgram {
    operations: Vec<AddressOp>,
    salt: u64,
    value_name: String,
}

impl AddressProgram {
    pub(super) fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        let count = rng.random_range(3..=7);
        let operations = (0..count)
            .map(|_| match rng.random_range(0..6) {
                0 => AddressOp::Add(rng.random()),
                1 => AddressOp::Sub(rng.random()),
                2 => AddressOp::Xor(rng.random()),
                3 => AddressOp::Multiply(rng.random::<u64>() | 1),
                4 => AddressOp::Rotate(rng.random_range(1..bits)),
                _ => AddressOp::XorShiftRight(rng.random_range(1..bits)),
            })
            .collect();
        Self {
            operations,
            salt: rng.random(),
            value_name: format!("v{:016x}", rng.random::<u64>()),
        }
    }

    fn generate_selector(rng: &mut impl Rng, bits: u32) -> Self {
        let count = rng.random_range(4..=7);
        let mut operations = vec![
            AddressOp::Multiply(rng.random::<u64>() | 1),
            AddressOp::XorShiftRight(rng.random_range(1..bits)),
        ];
        while operations.len() < count {
            operations.push(match rng.random_range(0..6) {
                0 => AddressOp::Add(rng.random()),
                1 => AddressOp::Sub(rng.random()),
                2 => AddressOp::Xor(rng.random()),
                3 => AddressOp::Multiply(rng.random::<u64>() | 1),
                4 => AddressOp::Rotate(rng.random_range(1..bits)),
                _ => AddressOp::XorShiftRight(rng.random_range(1..bits)),
            });
        }
        operations.shuffle(rng);
        Self {
            operations,
            salt: rng.random(),
            value_name: format!("v{:016x}", rng.random::<u64>()),
        }
    }

    fn constant<'ctx>(word: IntType<'ctx>, value: u64) -> IntValue<'ctx> {
        word.const_int(value, false)
    }

    fn rotate<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        shift: u32,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let left = builder.build_left_shift(value, Self::constant(word, u64::from(shift)), &self.value_name)?;
        let right = builder.build_right_shift(
            value,
            Self::constant(word, u64::from(word.get_bit_width() - shift)),
            false,
            &self.value_name,
        )?;
        Ok(builder.build_or(left, right, &self.value_name)?)
    }

    fn derive_key<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        base: IntValue<'ctx>,
        row: u32,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = base.get_type();
        let salted = builder.build_xor(base, Self::constant(word, self.salt), &self.value_name)?;
        builder
            .build_int_add(salted, Self::constant(word, u64::from(row)), &self.value_name)
            .map_err(Into::into)
    }

    fn emit_forward<'ctx>(&self, builder: &Builder<'ctx>, mut value: IntValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        for operation in &self.operations {
            value = match *operation {
                AddressOp::Add(key) => builder.build_int_add(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Sub(key) => builder.build_int_sub(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Xor(key) => builder.build_xor(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Multiply(key) => {
                    builder.build_int_mul(value, Self::constant(word, key), &self.value_name)?
                },
                AddressOp::Rotate(shift) => self.rotate(builder, value, shift)?,
                AddressOp::XorShiftRight(shift) => {
                    let shifted = builder.build_right_shift(
                        value,
                        Self::constant(word, u64::from(shift)),
                        false,
                        &self.value_name,
                    )?;
                    builder.build_xor(value, shifted, &self.value_name)?
                },
            };
        }
        Ok(value)
    }

    fn emit_inverse<'ctx>(&self, builder: &Builder<'ctx>, mut value: IntValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        for operation in self.operations.iter().rev() {
            value = match *operation {
                AddressOp::Add(key) => builder.build_int_sub(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Sub(key) => builder.build_int_add(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Xor(key) => builder.build_xor(value, Self::constant(word, key), &self.value_name)?,
                AddressOp::Multiply(key) => builder.build_int_mul(
                    value,
                    Self::constant(word, inverse_odd(key, word.get_bit_width())),
                    &self.value_name,
                )?,
                AddressOp::Rotate(shift) => self.rotate(builder, value, word.get_bit_width() - shift)?,
                AddressOp::XorShiftRight(shift) => {
                    let mut inverse = value;
                    let mut distance = shift;
                    while distance < word.get_bit_width() {
                        let shifted = builder.build_right_shift(
                            value,
                            Self::constant(word, u64::from(distance)),
                            false,
                            &self.value_name,
                        )?;
                        inverse = builder.build_xor(inverse, shifted, &self.value_name)?;
                        distance += shift;
                    }
                    inverse
                },
            };
        }
        Ok(value)
    }

    pub(super) fn encode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        address: IntValue<'ctx>,
        base: IntValue<'ctx>,
        row: u32,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let mixed = builder.build_xor(address, self.derive_key(builder, base, row)?, &self.value_name)?;
        self.emit_forward(builder, mixed)
    }

    pub(super) fn decode<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        encoded: IntValue<'ctx>,
        base: IntValue<'ctx>,
        row: u32,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let mixed = self.emit_inverse(builder, encoded)?;
        Ok(builder.build_xor(mixed, self.derive_key(builder, base, row)?, &self.value_name)?)
    }

    /// Recover the input for a small selector code without emitting a second
    /// IR program.  The resolver uses the forward address codec on its
    /// invocation-local selector; callers store this inverse preimage as the
    /// token carried through the frame nonce envelope.
    fn invert_constant(&self, mut value: u64, bits: u32) -> u64 {
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        value &= mask;
        for operation in self.operations.iter().rev() {
            value = match *operation {
                AddressOp::Add(key) => value.wrapping_sub(key),
                AddressOp::Sub(key) => value.wrapping_add(key),
                AddressOp::Xor(key) => value ^ key,
                AddressOp::Multiply(key) => value.wrapping_mul(inverse_odd(key, bits)),
                AddressOp::Rotate(shift) => {
                    let shift = shift % bits;
                    ((value >> shift) | (value << (bits - shift))) & mask
                },
                AddressOp::XorShiftRight(shift) => {
                    let mut inverse = value;
                    let mut distance = shift;
                    while distance < bits {
                        inverse ^= value >> distance;
                        distance += shift;
                    }
                    inverse
                },
            } & mask;
        }
        value
    }

    #[cfg(test)]
    fn apply_constant(&self, mut value: u64, bits: u32) -> u64 {
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        value &= mask;
        for operation in &self.operations {
            value = match *operation {
                AddressOp::Add(key) => value.wrapping_add(key),
                AddressOp::Sub(key) => value.wrapping_sub(key),
                AddressOp::Xor(key) => value ^ key,
                AddressOp::Multiply(key) => value.wrapping_mul(key),
                AddressOp::Rotate(shift) => {
                    let shift = shift % bits;
                    ((value << shift) | (value >> (bits - shift))) & mask
                },
                AddressOp::XorShiftRight(shift) => value ^ (value >> shift),
            } & mask;
        }
        value
    }
}

#[derive(Clone, Copy)]
enum StateOp {
    Add(u64),
    Sub(u64),
    Xor(u64),
    Multiply(u64),
    Rotate(u32),
    XorShiftRight(u32),
}

#[derive(Clone, Copy)]
enum WitnessMix {
    Xor,
    Add,
    Sub,
    FoldedXor { shift: u32 },
    RotatedXor { shift: u32 },
}

impl WitnessMix {
    fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        match rng.random_range(0..5) {
            0 => Self::Xor,
            1 => Self::Add,
            2 => Self::Sub,
            3 => Self::FoldedXor {
                shift: rng.random_range(1..bits),
            },
            _ => Self::RotatedXor {
                shift: rng.random_range(1..bits),
            },
        }
    }
}

#[derive(Clone, Copy)]
enum VariantSelector {
    LowBits,
    Rotated { shift: u32 },
    Folded { shift: u32 },
    Affine { multiplier: u64, salt: u64, shift: u32 },
}

impl VariantSelector {
    fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        match rng.random_range(0..4) {
            0 => Self::LowBits,
            1 => Self::Rotated {
                shift: rng.random_range(1..bits),
            },
            2 => Self::Folded {
                shift: rng.random_range(1..bits),
            },
            _ => Self::Affine {
                multiplier: rng.random::<u64>() | 1,
                salt: rng.random(),
                shift: rng.random_range(0..bits - 1),
            },
        }
    }

    fn kind(self) -> u8 {
        match self {
            Self::LowBits => 0,
            Self::Rotated { .. } => 1,
            Self::Folded { .. } => 2,
            Self::Affine { .. } => 3,
        }
    }

    fn apply(self, value: u64, bits: u32) -> u32 {
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        let value = value & mask;
        let mixed = match self {
            Self::LowBits => value,
            Self::Rotated { shift } => {
                let shift = shift % bits;
                ((value << shift) | (value >> (bits - shift))) & mask
            },
            Self::Folded { shift } => value ^ (value >> shift),
            Self::Affine {
                multiplier,
                salt,
                shift,
            } => value.wrapping_mul(multiplier).wrapping_add(salt) >> shift,
        };
        u32::try_from(mixed & u64::from(TARGET_VARIANTS - 1)).expect("variant is masked to two bits")
    }

    fn emit<'ctx>(self, builder: &Builder<'ctx>, state: IntValue<'ctx>, name: &str) -> anyhow::Result<IntValue<'ctx>> {
        let word = state.get_type();
        let bits = word.get_bit_width();
        let mixed = match self {
            Self::LowBits => state,
            Self::Rotated { shift } => {
                let left = builder.build_left_shift(state, word.const_int(u64::from(shift), false), name)?;
                let right =
                    builder.build_right_shift(state, word.const_int(u64::from(bits - shift), false), false, name)?;
                builder.build_or(left, right, name)?
            },
            Self::Folded { shift } => {
                let shifted = builder.build_right_shift(state, word.const_int(u64::from(shift), false), false, name)?;
                builder.build_xor(state, shifted, name)?
            },
            Self::Affine {
                multiplier,
                salt,
                shift,
            } => {
                let multiplied = builder.build_int_mul(state, word.const_int(multiplier, false), name)?;
                let added = builder.build_int_add(multiplied, word.const_int(salt, false), name)?;
                builder.build_right_shift(added, word.const_int(u64::from(shift), false), false, name)?
            },
        };
        Ok(builder.build_and(mixed, word.const_int(u64::from(TARGET_VARIANTS - 1), false), name)?)
    }
}

#[derive(Clone, Copy)]
struct VariantLayout {
    multiplier: u32,
    salt: u32,
}

impl VariantLayout {
    fn generate(rng: &mut impl Rng) -> Self {
        Self {
            multiplier: rng.random_range(0..4) | 1,
            salt: rng.random_range(0..4),
        }
    }

    fn physical(self, logical: u32) -> u32 {
        logical.wrapping_mul(self.multiplier).wrapping_add(self.salt) & (TARGET_VARIANTS - 1)
    }

    fn emit<'ctx>(
        self,
        builder: &Builder<'ctx>,
        logical: IntValue<'ctx>,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = logical.get_type();
        let multiplied = builder.build_int_mul(logical, word.const_int(u64::from(self.multiplier), false), name)?;
        let added = builder.build_int_add(multiplied, word.const_int(u64::from(self.salt), false), name)?;
        Ok(builder.build_and(added, word.const_int(u64::from(TARGET_VARIANTS - 1), false), name)?)
    }
}

/// Per-transfer state transition. The transition is bijective for a fixed
/// edge index, but the index is mixed into it so the next variant depends on
/// both the path state and the selected successor. Its operation sequence is
/// generated independently for each transfer.
pub(super) struct StateProgram {
    operations: Vec<StateOp>,
    salt: u64,
    witness_mix: WitnessMix,
    value_name: String,
}

impl StateProgram {
    pub(super) fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        Self {
            operations: (0..rng.random_range(2..=6))
                .map(|_| match rng.random_range(0..6) {
                    0 => StateOp::Add(rng.random()),
                    1 => StateOp::Sub(rng.random()),
                    2 => StateOp::Xor(rng.random()),
                    3 => StateOp::Multiply(rng.random::<u64>() | 1),
                    4 => StateOp::Rotate(rng.random_range(1..bits)),
                    _ => StateOp::XorShiftRight(rng.random_range(1..bits)),
                })
                .collect(),
            salt: rng.random(),
            witness_mix: WitnessMix::generate(rng, bits),
            value_name: format!("v{:016x}", rng.random::<u64>()),
        }
    }

    fn normalize_witness<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        witness: IntValue<'ctx>,
        word: IntType<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let witness_bits = witness.get_type().get_bit_width();
        let word_bits = word.get_bit_width();
        if witness_bits == word_bits {
            Ok(witness)
        } else if witness_bits < word_bits {
            Ok(builder.build_int_z_extend(witness, word, &self.value_name)?)
        } else {
            Ok(builder.build_int_truncate(witness, word, &self.value_name)?)
        }
    }

    fn mix_value<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        input: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        Ok(match self.witness_mix {
            WitnessMix::Xor => builder.build_xor(value, input, &self.value_name)?,
            WitnessMix::Add => builder.build_int_add(value, input, &self.value_name)?,
            WitnessMix::Sub => builder.build_int_sub(value, input, &self.value_name)?,
            WitnessMix::FoldedXor { shift } => {
                let shifted = builder.build_right_shift(
                    input,
                    word.const_int(u64::from(shift), false),
                    false,
                    &self.value_name,
                )?;
                let folded = builder.build_xor(input, shifted, &self.value_name)?;
                builder.build_xor(value, folded, &self.value_name)?
            },
            WitnessMix::RotatedXor { shift } => {
                let left =
                    builder.build_left_shift(input, word.const_int(u64::from(shift), false), &self.value_name)?;
                let right = builder.build_right_shift(
                    input,
                    word.const_int(u64::from(word.get_bit_width() - shift), false),
                    false,
                    &self.value_name,
                )?;
                let rotated = builder.build_or(left, right, &self.value_name)?;
                builder.build_xor(value, rotated, &self.value_name)?
            },
        })
    }

    pub(super) fn update<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        state: IntValue<'ctx>,
        index: IntValue<'ctx>,
        witnesses: &[IntValue<'ctx>],
        frame_nonce: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = state.get_type();
        let index = if index.get_type() == word {
            index
        } else {
            builder.build_int_z_extend(index, word, &self.value_name)?
        };
        let mut value = builder.build_xor(state, index, &self.value_name)?;
        value = builder.build_xor(value, word.const_int(self.salt, false), &self.value_name)?;
        // Couple every transition to the invocation's stack identity. This is
        // a runtime nonce, not a secret: it prevents a static pass from
        // treating the state machine as a pure constant recurrence while
        // preserving the same value throughout a recursive invocation.
        let frame_nonce = self.normalize_witness(builder, frame_nonce, word)?;
        value = self.mix_value(builder, value, frame_nonce)?;
        for witness in witnesses {
            let witness = self.normalize_witness(builder, *witness, word)?;
            value = self.mix_value(builder, value, witness)?;
        }
        for operation in &self.operations {
            value = match *operation {
                StateOp::Add(key) => builder.build_int_add(value, word.const_int(key, false), &self.value_name)?,
                StateOp::Sub(key) => builder.build_int_sub(value, word.const_int(key, false), &self.value_name)?,
                StateOp::Xor(key) => builder.build_xor(value, word.const_int(key, false), &self.value_name)?,
                StateOp::Multiply(key) => builder.build_int_mul(value, word.const_int(key, false), &self.value_name)?,
                StateOp::Rotate(shift) => {
                    let left =
                        builder.build_left_shift(value, word.const_int(u64::from(shift), false), &self.value_name)?;
                    let right = builder.build_right_shift(
                        value,
                        word.const_int(u64::from(word.get_bit_width() - shift), false),
                        false,
                        &self.value_name,
                    )?;
                    builder.build_or(left, right, &self.value_name)?
                },
                StateOp::XorShiftRight(shift) => {
                    let shifted = builder.build_right_shift(
                        value,
                        word.const_int(u64::from(shift), false),
                        false,
                        &self.value_name,
                    )?;
                    builder.build_xor(value, shifted, &self.value_name)?
                },
            };
        }
        Ok(value)
    }
}

fn inverse_odd(value: u64, bits: u32) -> u64 {
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut inverse = 1u64;
    for _ in 0..bits.ilog2().saturating_add(1) {
        inverse = inverse.wrapping_mul(2u64.wrapping_sub(value.wrapping_mul(inverse))) & mask;
    }
    inverse
}

/// An injective bucket mapping for the generated local edge IDs. Buckets are
/// deliberately only half occupied; lookup needs neither probing nor a branch.
#[derive(Clone, Copy)]
enum HashFamily {
    Affine,
    FoldedXorShift { shift: u32 },
}

#[derive(Clone, Copy)]
enum TargetFamily {
    Direct,
    Reverse,
    Rekeyed,
    Rotated { shift: u32 },
}

/// Store one target displacement as two independently keyed shares.  The
/// reconstruction family is selected per target address and carried in the
/// reversible gateway tag envelope, so a row does not expose one universal
/// share formula for all of its target cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShareFamily {
    XorAdd,
    AddXor,
}

impl ShareFamily {
    fn generate(rng: &mut impl Rng) -> Self {
        if rng.random_bool(0.5) {
            Self::XorAdd
        } else {
            Self::AddXor
        }
    }

    fn code(self) -> u64 {
        match self {
            Self::XorAdd => 0,
            Self::AddXor => 1,
        }
    }

    fn pack_tag(self, tag: u64, bits: u32) -> u64 {
        let payload_mask = (1u64 << (bits - 1)) - 1;
        (tag & payload_mask) | (self.code() << (bits - 1))
    }

    fn unpack_tag(packed: u64, bits: u32) -> (u64, Self) {
        let family = if (packed >> (bits - 1)) & 1 == 0 {
            Self::XorAdd
        } else {
            Self::AddXor
        };
        let payload_mask = (1u64 << (bits - 1)) - 1;
        (packed & payload_mask, family)
    }
}

impl TargetFamily {
    fn generate(rng: &mut impl Rng, bits: u32) -> Self {
        match rng.random_range(0..4) {
            0 => Self::Direct,
            1 => Self::Reverse,
            2 => Self::Rekeyed,
            _ => Self::Rotated {
                shift: rng.random_range(1..bits),
            },
        }
    }
}

struct HashLayout {
    buckets: u32,
    salt: u64,
    multiplier: u64,
    family: HashFamily,
}

impl HashLayout {
    fn generate(count: usize, rng: &mut impl Rng) -> anyhow::Result<Self> {
        ensure!(count != 0, "VM hash row is empty");
        let buckets = u32::try_from(count)?
            .checked_next_power_of_two()
            .and_then(|size| size.checked_mul(2))
            .context("VM hash row is too large")?;
        for _ in 0..64 {
            let family = if rng.random_bool(1.0 / 2.0) {
                HashFamily::Affine
            } else {
                HashFamily::FoldedXorShift {
                    shift: rng.random_range(1..32),
                }
            };
            let layout = Self {
                buckets,
                salt: rng.random(),
                multiplier: rng.random::<u64>() | 1,
                family,
            };
            let slots: HashSet<_> = (0..u32::try_from(count)?).map(|index| layout.slot(index)).collect();
            if slots.len() == count {
                return Ok(layout);
            }
        }
        anyhow::bail!("VM hash family failed to produce an injective layout")
    }

    fn hash_input(&self, index: u64) -> u64 {
        match self.family {
            HashFamily::Affine => index,
            HashFamily::FoldedXorShift { shift } => index ^ (index >> shift),
        }
    }

    fn slot(&self, index: u32) -> u32 {
        let bucket = self
            .hash_input(u64::from(index))
            .wrapping_mul(self.multiplier)
            .wrapping_add(self.salt)
            & u64::from(self.buckets - 1);
        1 + u32::try_from(bucket).expect("bucket is masked to a u32 range")
    }

    fn tag(&self, index: u32) -> u64 {
        u64::from(index) ^ self.salt
    }
}

struct Records<'ctx> {
    pointer: PointerValue<'ctx>,
    ty: ArrayType<'ctx>,
    permutation_pointer: PointerValue<'ctx>,
    permutation_ty: ArrayType<'ctx>,
    word: IntType<'ctx>,
    // Each decoder variant gets an invocation-local permutation of the four
    // logical values (hash tag, encoded displacement, static key, and gateway
    // tag) inside a wider physical record. The permutation is emitted as a
    // branch-free Fisher-Yates network, so the physical field is not an affine
    // rotation that a static pass can normalize by subtracting one residue.
    // The encoded displacement also has one invocation-dependent spill slot;
    // its choice is derived from the frame nonce in `cell`, so a static pass
    // cannot treat every record access as the same fixed logical column.
    frame_nonce: IntValue<'ctx>,
    row_salt: u64,
    variant_salt: u64,
    permutation_salt: u64,
    value_name: String,
}

impl<'ctx> Records<'ctx> {
    fn permuted_field(
        &self,
        builder: &Builder<'ctx>,
        field: u32,
        variant: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = self.word;
        let variant_salt =
            builder.build_int_mul(variant, word.const_int(self.variant_salt, false), &self.value_name)?;
        let mut seed = builder.build_xor(self.frame_nonce, variant_salt, &self.value_name)?;
        seed = builder.build_int_add(seed, word.const_int(self.row_salt, false), &self.value_name)?;
        seed = builder.build_xor(
            seed,
            builder.build_right_shift(seed, word.const_int(17, false), false, &self.value_name)?,
            &self.value_name,
        )?;
        seed = builder.build_int_add(seed, word.const_int(self.permutation_salt, false), &self.value_name)?;

        let mut position = word.const_int(u64::from(field), false);
        // Fisher-Yates for four entries, specialized to one queried element.
        // Tracking only the queried position avoids materializing a table while
        // retaining a true permutation for every runtime seed.
        for (stage, (last, modulus)) in [(0u32, (3u32, 4u32)), (1, (2, 3)), (2, (1, 2))] {
            let swap_with =
                builder.build_int_unsigned_rem(seed, word.const_int(u64::from(modulus), false), &self.value_name)?;
            let at_last = builder.build_int_compare(
                IntPredicate::EQ,
                position,
                word.const_int(u64::from(last), false),
                &self.value_name,
            )?;
            let at_swap = builder.build_int_compare(IntPredicate::EQ, position, swap_with, &self.value_name)?;
            let swapped_from_other = builder
                .build_select(
                    at_swap,
                    word.const_int(u64::from(last), false),
                    position,
                    &self.value_name,
                )?
                .into_int_value();
            // Both comparisons use the original position. In particular, a
            // position equal to `last` must not be swapped twice when the
            // selected partner is different.
            position = builder
                .build_select(at_last, swap_with, swapped_from_other, &self.value_name)?
                .into_int_value();

            let shift = 7 + stage * 5;
            let shifted =
                builder.build_right_shift(seed, word.const_int(u64::from(shift), false), false, &self.value_name)?;
            seed = builder.build_xor(seed, shifted, &self.value_name)?;
            seed = builder.build_int_add(
                seed,
                word.const_int(self.row_salt.rotate_left(stage + 1), false),
                &self.value_name,
            )?;
        }
        Ok(position)
    }

    fn cell(
        &self,
        builder: &Builder<'ctx>,
        slot: IntValue<'ctx>,
        field: u32,
        variant: IntValue<'ctx>,
    ) -> anyhow::Result<PointerValue<'ctx>> {
        let field = usize::try_from(field).context("VM record field is out of range")?;
        ensure!(field < 4, "VM record field is out of range");
        let word = self.word;
        let permutation_cell = builder.build_in_bounds_gep2(
            self.permutation_ty,
            self.permutation_pointer,
            &[word.const_zero(), variant, word.const_int(u64::try_from(field)?, false)],
            &self.value_name,
        )?;
        let physical_field = builder.build_load2(word, permutation_cell, &self.value_name)?;
        physical_field
            .as_instruction_value()
            .context("VM record permutation load")?
            .set_volatile(true)?;
        let physical_field = physical_field.into_int_value();
        let physical_field = if field == 1 {
            // Keep the normal four-field permutation as the first candidate,
            // but route the encoded displacement to an extra physical slot on
            // half of invocations. The same nonce is available during record
            // initialization and every later load/store, so the mapping is
            // stable for one call while remaining runtime-dependent.
            let shifted_nonce =
                builder.build_right_shift(self.frame_nonce, word.const_int(5, false), false, &self.value_name)?;
            let folded_nonce = builder.build_xor(self.frame_nonce, shifted_nonce, &self.value_name)?;
            let nonce_bit = builder.build_and(folded_nonce, word.const_int(1, false), &self.value_name)?;
            let use_spill =
                builder.build_int_compare(IntPredicate::EQ, nonce_bit, word.const_int(1, false), &self.value_name)?;
            builder
                .build_select(use_spill, word.const_int(4, false), physical_field, &self.value_name)?
                .into_int_value()
        } else {
            physical_field
        };
        Ok(builder.build_in_bounds_gep2(
            self.ty,
            self.pointer,
            &[word.const_zero(), slot, physical_field],
            &self.value_name,
        )?)
    }

    fn initialize_permutations(&self, builder: &Builder<'ctx>) -> anyhow::Result<()> {
        let word = self.word;
        for variant in 0..TARGET_VARIANTS {
            let variant = word.const_int(u64::from(variant), false);
            for field in 0..4u32 {
                let physical_field = self.permuted_field(builder, field, variant)?;
                let permutation_cell = builder.build_in_bounds_gep2(
                    self.permutation_ty,
                    self.permutation_pointer,
                    &[word.const_zero(), variant, word.const_int(u64::from(field), false)],
                    &self.value_name,
                )?;
                builder
                    .build_store(permutation_cell, physical_field)?
                    .set_volatile(true)?;
            }
        }
        Ok(())
    }

    fn load(
        &self,
        builder: &Builder<'ctx>,
        slot: IntValue<'ctx>,
        field: u32,
        variant: IntValue<'ctx>,
        _name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let loaded = builder.build_load2(self.word, self.cell(builder, slot, field, variant)?, &self.value_name)?;
        loaded
            .as_instruction_value()
            .context("VM record load")?
            .set_volatile(true)?;
        Ok(loaded.into_int_value())
    }

    fn store(
        &self,
        builder: &Builder<'ctx>,
        slot: IntValue<'ctx>,
        field: u32,
        variant: IntValue<'ctx>,
        value: IntValue<'ctx>,
    ) -> anyhow::Result<()> {
        builder
            .build_store(self.cell(builder, slot, field, variant)?, value)?
            .set_volatile(true)?;
        Ok(())
    }
}

struct TargetPool<'ctx> {
    pointer: PointerValue<'ctx>,
    ty: ArrayType<'ctx>,
    word: IntType<'ctx>,
    addresses: Vec<PointerValue<'ctx>>,
    slots: Vec<u32>,
    value_name: String,
}

fn pool_layout(pool_count: u32, rng: &mut impl Rng) -> anyhow::Result<(u32, Vec<u32>)> {
    ensure!(pool_count > 0, "VM target pool is empty");
    let gap_count = rng.random_range(1..=pool_count.min(8));
    let pool_len = pool_count
        .checked_add(gap_count)
        .and_then(|count| count.checked_add(1))
        .context("VM target pool is too large")?;
    Ok((pool_len, pool_slots(pool_count, pool_len, rng)?))
}

fn pool_slots(pool_count: u32, pool_len: u32, rng: &mut impl Rng) -> anyhow::Result<Vec<u32>> {
    ensure!(pool_count > 0, "VM target pool is empty");
    ensure!(pool_len > pool_count, "VM target pool has no room for all targets");
    let mut slots: Vec<_> = (1..pool_len).collect();
    slots.shuffle(rng);
    slots.truncate(usize::try_from(pool_count)?);
    Ok(slots)
}

impl<'ctx> TargetPool<'ctx> {
    fn cell(&self, builder: &Builder<'ctx>, index: IntValue<'ctx>) -> anyhow::Result<PointerValue<'ctx>> {
        Ok(builder.build_in_bounds_gep2(
            self.ty,
            self.pointer,
            &[self.word.const_zero(), index],
            &self.value_name,
        )?)
    }

    fn load(&self, builder: &Builder<'ctx>, index: IntValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        let loaded = builder.build_load2(self.word, self.cell(builder, index)?, &self.value_name)?;
        loaded
            .as_instruction_value()
            .context("VM target pool load")?
            .set_volatile(true)?;
        Ok(loaded.into_int_value())
    }

    fn store(&self, builder: &Builder<'ctx>, index: IntValue<'ctx>, value: IntValue<'ctx>) -> anyhow::Result<()> {
        builder
            .build_store(self.cell(builder, index)?, value)?
            .set_volatile(true)?;
        Ok(())
    }
}

struct HashRow<'ctx> {
    records: Records<'ctx>,
    buckets: u32,
    family: HashFamily,
    selector: VariantSelector,
    variant_layout: VariantLayout,
    target_family: TargetFamily,
    decode_mode: DecodeMode,
    programs: Vec<AddressProgram>,
    /// A row-local target pool is split into a small number of shards.  The
    /// shard is selected from the logical edge index, so no global target
    /// table or row-wide key/anchor invariant is emitted.
    domains: Vec<TargetDomain<'ctx>>,
    shard_count: u32,
    pool_len: u32,
}

pub(super) struct TargetLookup<'ctx> {
    pub(super) pointer: PointerValue<'ctx>,
    pub(super) gateway_tag: IntValue<'ctx>,
}

/// Keep two decoder envelopes available so one function does not expose one
/// universal "decode every variant, then select" shape. Both modes read the
/// same row-local record and produce the same pool index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecodeMode {
    CandidateSelect,
    SharedProgram,
}

/// Address materialization is local to one transfer row. A shared
/// pool/key/anchor creates a strong function-wide clustering invariant for a
/// static pass; independent domains remove that invariant while preserving
/// the legal LLVM target set.
struct TargetDomain<'ctx> {
    key: PointerValue<'ctx>,
    anchor_value: PointerValue<'ctx>,
    pointer: PointerType<'ctx>,
    pool: TargetPool<'ctx>,
    share_pool: TargetPool<'ctx>,
}

impl<'ctx> TargetDomain<'ctx> {
    fn pool_index(&self, address: PointerValue<'ctx>) -> anyhow::Result<u64> {
        self.pool
            .addresses
            .iter()
            .position(|candidate| candidate.as_value_ref() == address.as_value_ref())
            .map(|position| u64::from(self.pool.slots[position]))
            .context("VM target is missing from target pool")
    }
}

pub(super) struct TargetTable<'ctx> {
    rows: Vec<HashRow<'ctx>>,
    word: IntType<'ctx>,
    frame_nonce: IntValue<'ctx>,
    frame_pointer: PointerValue<'ctx>,
    value_name: String,
    target_resolvers: HashMap<usize, TargetResolver<'ctx>>,
}

#[derive(Clone, Copy)]
struct TargetResolver<'ctx> {
    helper: FunctionValue<'ctx>,
    selector: u64,
    bucket: PointerValue<'ctx>,
    abi: TargetResolverAbi,
}

#[derive(Clone, Copy)]
pub(super) enum TargetResolverAbi {
    NonceFrameSelectorBucket,
    FrameNonceBucketSelector,
}

impl<'ctx> TargetTable<'ctx> {
    pub(super) fn word_type(module: &Module<'ctx>, function: FunctionValue<'ctx>) -> Option<IntType<'ctx>> {
        let layout = module.get_data_layout();
        let layout = layout.as_str().to_str().ok()?;
        if function
            .as_global_value()
            .as_pointer_value()
            .get_type()
            .get_address_space()
            != AddressSpace::default()
            || layout.split('-').any(|part| {
                (part.starts_with(['A', 'P', 'G']) && !matches!(part, "A0" | "P0" | "G0"))
                    || part.starts_with("pu")
                    || part.starts_with("pe")
            })
        {
            return None;
        }
        let data = TargetData::create(layout);
        let word = module.get_context().ptr_sized_int_type(&data, None);
        matches!(word.get_bit_width(), 32 | 64).then_some(word)
    }

    pub(super) fn create(
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        word: IntType<'ctx>,
        addresses: &[Vec<PointerValue<'ctx>>],
        frame_nonce: IntValue<'ctx>,
        frame_pointer: PointerValue<'ctx>,
        rng: &mut impl Rng,
    ) -> anyhow::Result<Self> {
        ensure!(addresses.iter().all(|row| !row.is_empty()), "VM hash row is empty");
        let mut table = Self {
            rows: Vec::with_capacity(addresses.len()),
            word,
            frame_nonce,
            frame_pointer,
            value_name: format!("v{:016x}", rng.random::<u64>()),
            target_resolvers: HashMap::new(),
        };
        table.target_resolvers = table.create_target_resolver_sources(module, builder, addresses, rng)?;
        Ok(table)
    }

    fn create_target_resolver_sources(
        &self,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        address_rows: &[Vec<PointerValue<'ctx>>],
        rng: &mut impl Rng,
    ) -> anyhow::Result<HashMap<usize, TargetResolver<'ctx>>> {
        let mut addresses = Vec::new();
        let mut seen = HashSet::new();
        for address in address_rows.iter().flatten() {
            let key = address.as_value_ref() as usize;
            if seen.insert(key) {
                addresses.push(*address);
            }
        }
        ensure!(!addresses.is_empty(), "VM target resolver address set is empty");
        addresses.shuffle(rng);
        let mut sources = HashMap::new();
        let mut cursor = 0usize;
        let mut resolver_index = 0usize;
        while cursor < addresses.len() {
            let remaining = addresses.len() - cursor;
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
            // Do not let a random four-way choice leave a final singleton.
            // A one-target resolver recreates the helper-to-blockaddress
            // relation this pool is meant to blur, so use a three-way bucket
            // when five entries remain and the four-way branch would leave
            // one address behind.
            let bucket_len = if remaining > 1 && remaining - bucket_len == 1 {
                3
            } else {
                bucket_len
            };
            let take = remaining.min(bucket_len);
            let bucket_entries = &addresses[cursor..cursor + take];
            let mut bucket_addresses = bucket_entries.to_vec();
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
                self.create_target_bucket_materializer(module, bucket_addresses.len(), abi, rng, &self.value_name)?;
            let bucket = self.create_target_bucket_vault(module, builder, &bucket_addresses, rng, &self.value_name)?;
            for (selector, address) in bucket_entries.iter().enumerate() {
                sources.insert(
                    address.as_value_ref() as usize,
                    TargetResolver {
                        helper,
                        selector: *selector_tokens
                            .get(selector)
                            .context("VM target resolver selector is missing")?,
                        bucket,
                        abi,
                    },
                );
            }
            cursor += take;
            resolver_index += 1;
        }
        Ok(sources)
    }

    /// Allocate invocation-local target storage initialized outside the VM body.
    pub(super) fn create_target_bucket_vault(
        &self,
        module: &Module<'ctx>,
        builder: &Builder<'ctx>,
        addresses: &[PointerValue<'ctx>],
        rng: &mut impl Rng,
        value_name: &str,
    ) -> anyhow::Result<PointerValue<'ctx>> {
        let bucket_len = u32::try_from(addresses.len())?;
        let bucket_type = self.word.array_type(bucket_len);
        let bucket = builder.build_alloca(bucket_type, value_name)?;
        // Keep blockaddress constants out of both the VM function and the
        // generic runtime resolver.  A private initializer owns the static
        // materialization and only receives the caller-owned bucket storage.
        // This leaves the resolver with a pure runtime integer interface while
        // preserving the exact target values required by indirectbr.
        let context = module.get_context();
        let initializer_type = context
            .void_type()
            .fn_type(&[self.frame_pointer.get_type().into()], false);
        let initializer = module.add_function(
            &format!("v{:016x}", rng.random::<u64>()),
            initializer_type,
            Some(Linkage::Private),
        );
        let done = done_attribute_name(initializer);
        initializer.add_attribute(AttributeLoc::Function, context.create_string_attribute(&done, "1"));
        initializer.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("amice.bcf.done", "1"),
        );
        for attribute in ["noinline", "optnone"] {
            initializer.add_attribute(
                AttributeLoc::Function,
                context.create_enum_attribute(Attribute::get_named_enum_kind_id(attribute), 0),
            );
        }
        let block = context.append_basic_block(initializer, &format!("v{:016x}", rng.random::<u64>()));
        let initializer_builder = context.create_builder();
        initializer_builder.position_at_end(block);
        let bucket_param = initializer
            .get_first_param()
            .context("VM target bucket initializer has no bucket")?
            .into_pointer_value();
        for (index, address) in addresses.iter().enumerate() {
            let cell = initializer_builder.build_in_bounds_gep2(
                bucket_type,
                bucket_param,
                &[
                    self.word.const_zero(),
                    self.word.const_int(u64::try_from(index)?, false),
                ],
                value_name,
            )?;
            let raw = initializer_builder.build_ptr_to_int(*address, self.word, value_name)?;
            initializer_builder.build_store(cell, raw)?.set_volatile(true)?;
        }
        initializer_builder.build_return(None)?;
        builder.build_call(initializer, &[bucket.into()], "")?;
        Ok(bucket)
    }

    /// Create a resolver for a small bucket of runtime target integers.
    ///
    /// Blockaddress materialization is kept in a caller-owned vault. The
    /// resolver only loads and permutes runtime integers, so its body no
    /// longer advertises a direct blockaddress-to-selector relation.
    pub(super) fn create_target_bucket_materializer(
        &self,
        module: &Module<'ctx>,
        bucket_len: usize,
        abi: TargetResolverAbi,
        rng: &mut impl Rng,
        value_name: &str,
    ) -> anyhow::Result<(FunctionValue<'ctx>, Vec<u64>)> {
        ensure!(
            (1..=4).contains(&bucket_len),
            "VM target resolver buckets must contain one, two, three or four addresses"
        );
        let helper_type = match abi {
            TargetResolverAbi::NonceFrameSelectorBucket => self.word.fn_type(
                &[
                    self.word.into(),
                    self.frame_pointer.get_type().into(),
                    self.word.into(),
                    self.frame_pointer.get_type().into(),
                ],
                false,
            ),
            TargetResolverAbi::FrameNonceBucketSelector => self.word.fn_type(
                &[
                    self.frame_pointer.get_type().into(),
                    self.word.into(),
                    self.frame_pointer.get_type().into(),
                    self.word.into(),
                ],
                false,
            ),
        };
        let helper = module.add_function(
            &format!("v{:016x}", rng.random::<u64>()),
            helper_type,
            Some(Linkage::Private),
        );
        let context = module.get_context();
        let done = done_attribute_name(helper);
        helper.add_attribute(AttributeLoc::Function, context.create_string_attribute(&done, "1"));
        helper.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("amice.bcf.done", "1"),
        );
        for attribute in ["noinline", "optnone"] {
            helper.add_attribute(
                AttributeLoc::Function,
                context.create_enum_attribute(Attribute::get_named_enum_kind_id(attribute), 0),
            );
        }
        let block = context.append_basic_block(helper, &format!("v{:016x}", rng.random::<u64>()));
        let helper_builder = context.create_builder();
        helper_builder.position_at_end(block);
        let (nonce, frame_pointer, encoded_selector, bucket) = match abi {
            TargetResolverAbi::NonceFrameSelectorBucket => (
                helper
                    .get_first_param()
                    .context("VM target bucket helper has no frame nonce")?
                    .into_int_value(),
                helper
                    .get_nth_param(1)
                    .context("VM target bucket helper has no frame pointer")?
                    .into_pointer_value(),
                helper
                    .get_nth_param(2)
                    .context("VM target bucket helper has no selector")?
                    .into_int_value(),
                helper
                    .get_nth_param(3)
                    .context("VM target bucket helper has no bucket")?
                    .into_pointer_value(),
            ),
            TargetResolverAbi::FrameNonceBucketSelector => (
                helper
                    .get_nth_param(1)
                    .context("VM target bucket helper has no frame nonce")?
                    .into_int_value(),
                helper
                    .get_first_param()
                    .context("VM target bucket helper has no frame pointer")?
                    .into_pointer_value(),
                helper
                    .get_nth_param(3)
                    .context("VM target bucket helper has no selector")?
                    .into_int_value(),
                helper
                    .get_nth_param(2)
                    .context("VM target bucket helper has no bucket")?
                    .into_pointer_value(),
            ),
        };
        let selector = helper_builder.build_xor(encoded_selector, nonce, value_name)?;
        let selector_domain = if bucket_len <= 2 { 2 } else { 4 };
        let selector_program = AddressProgram::generate_selector(rng, self.word.get_bit_width());
        let selector_value = selector_program.emit_forward(&helper_builder, selector)?;
        let selector_low = helper_builder.build_and(
            selector_value,
            self.word.const_int(u64::try_from(selector_domain - 1)?, false),
            value_name,
        )?;
        let bucket_type = self.word.array_type(u32::try_from(bucket_len)?);
        let mut raw_values = Vec::with_capacity(bucket_len);
        for index in 0..bucket_len {
            let cell = helper_builder.build_in_bounds_gep2(
                bucket_type,
                bucket,
                &[
                    self.word.const_zero(),
                    self.word.const_int(u64::try_from(index)?, false),
                ],
                value_name,
            )?;
            let raw = helper_builder
                .build_load2(self.word, cell, value_name)?
                .into_int_value();
            raw.as_instruction_value()
                .context("VM target bucket value load has no instruction")?
                .set_volatile(true)?;
            raw_values.push(raw);
        }
        let mut selector_slots: Vec<u32> = (0..u32::try_from(bucket_len)?).collect();
        selector_slots.shuffle(rng);
        while selector_slots.len() < selector_domain {
            selector_slots.push(0);
        }
        let mut selected = raw_values[usize::try_from(selector_slots[0])?];
        for (code, slot) in selector_slots.iter().enumerate().skip(1) {
            let predicate = helper_builder.build_int_compare(
                IntPredicate::EQ,
                selector_low,
                self.word.const_int(u64::try_from(code)?, false),
                value_name,
            )?;
            selected = helper_builder
                .build_select(predicate, raw_values[usize::try_from(*slot)?], selected, value_name)?
                .into_int_value();
        }
        let lane = helper_builder
            .build_load(self.word, frame_pointer, value_name)?
            .into_int_value();
        lane.as_instruction_value()
            .context("VM target bucket lane load has no instruction")?
            .set_volatile(true)?;
        let encoded_nonce = helper_builder.build_int_add(nonce, lane, value_name)?;
        let encoded = helper_builder.build_int_add(selected, encoded_nonce, value_name)?;
        helper_builder.build_return(Some(&encoded))?;
        let mut selector_tokens = vec![0u64; bucket_len];
        for (code, slot) in selector_slots.iter().enumerate() {
            let slot = usize::try_from(*slot)?;
            if slot < selector_tokens.len() {
                selector_tokens[slot] =
                    selector_program.invert_constant(u64::try_from(code)?, self.word.get_bit_width());
            }
        }
        Ok((helper, selector_tokens))
    }

    pub(super) fn call_target_bucket_materializer(
        &self,
        builder: &Builder<'ctx>,
        helper: FunctionValue<'ctx>,
        selector: IntValue<'ctx>,
        bucket: PointerValue<'ctx>,
        abi: TargetResolverAbi,
        value_name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let arguments = match abi {
            TargetResolverAbi::NonceFrameSelectorBucket => [
                self.frame_nonce.into(),
                self.frame_pointer.into(),
                selector.into(),
                bucket.into(),
            ],
            TargetResolverAbi::FrameNonceBucketSelector => [
                self.frame_pointer.into(),
                self.frame_nonce.into(),
                bucket.into(),
                selector.into(),
            ],
        };
        let call = builder.build_call(helper, &arguments, value_name)?;
        let encoded = call
            .try_as_basic_value()
            .basic()
            .context("VM target bucket helper did not return an integer")?
            .into_int_value();
        let lane = builder
            .build_load(self.word, self.frame_pointer, value_name)?
            .into_int_value();
        lane.as_instruction_value()
            .context("VM target bucket caller lane load has no instruction")?
            .set_volatile(true)?;
        let encoded_nonce = builder.build_int_add(self.frame_nonce, lane, value_name)?;
        Ok(builder.build_int_sub(encoded, encoded_nonce, value_name)?)
    }

    /// Select between two runtime target values using invocation state.
    ///
    /// The caller supplies both pointers from a volatile address vault.  Keeping
    /// the block-address materialization out of this helper prevents every
    /// continuation from exposing the same `ptrtoint(blockaddress(...))`
    /// pattern while preserving the legal destination set of its `indirectbr`.
    pub(super) fn create_continuation_materializer(
        &self,
        module: &Module<'ctx>,
        target_type: PointerType<'ctx>,
        choice_slot: PointerValue<'ctx>,
        rng: &mut impl Rng,
        value_name: &str,
    ) -> anyhow::Result<FunctionValue<'ctx>> {
        let bool_type = module.get_context().bool_type();
        let helper_type = self.word.fn_type(
            &[
                self.word.into(),
                self.frame_pointer.get_type().into(),
                choice_slot.get_type().into(),
                target_type.into(),
                target_type.into(),
            ],
            false,
        );
        let helper = module.add_function(
            &format!("v{:016x}", rng.random::<u64>()),
            helper_type,
            Some(Linkage::Private),
        );
        let context = module.get_context();
        let done = done_attribute_name(helper);
        helper.add_attribute(AttributeLoc::Function, context.create_string_attribute(&done, "1"));
        helper.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("amice.bcf.done", "1"),
        );
        for attribute in ["noinline", "optnone"] {
            helper.add_attribute(
                AttributeLoc::Function,
                context.create_enum_attribute(Attribute::get_named_enum_kind_id(attribute), 0),
            );
        }
        let block = context.append_basic_block(helper, &format!("v{:016x}", rng.random::<u64>()));
        let helper_builder = context.create_builder();
        helper_builder.position_at_end(block);
        let nonce = helper
            .get_first_param()
            .context("VM continuation helper has no frame nonce")?
            .into_int_value();
        let frame_pointer = helper
            .get_nth_param(1)
            .context("VM continuation helper has no frame pointer")?
            .into_pointer_value();
        let choice_pointer = helper
            .get_nth_param(2)
            .context("VM continuation helper has no choice pointer")?
            .into_pointer_value();
        let choice = helper_builder
            .build_load(bool_type, choice_pointer, value_name)?
            .into_int_value();
        choice
            .as_instruction_value()
            .context("VM continuation choice load has no instruction")?
            .set_volatile(true)?;
        let target = helper
            .get_nth_param(3)
            .context("VM continuation helper has no target pointer")?
            .into_pointer_value();
        let retry = helper
            .get_nth_param(4)
            .context("VM continuation helper has no retry pointer")?
            .into_pointer_value();
        let target_value = helper_builder.build_ptr_to_int(target, self.word, value_name)?;
        let retry_value = helper_builder.build_ptr_to_int(retry, self.word, value_name)?;
        let selected = helper_builder
            .build_select(choice, retry_value, target_value, value_name)?
            .into_int_value();
        let lane = helper_builder
            .build_load(self.word, frame_pointer, value_name)?
            .into_int_value();
        lane.as_instruction_value()
            .context("VM continuation helper lane load has no instruction")?
            .set_volatile(true)?;
        let encoded_nonce = helper_builder.build_int_add(nonce, lane, value_name)?;
        let encoded = helper_builder.build_int_add(selected, encoded_nonce, value_name)?;
        helper_builder.build_return(Some(&encoded))?;
        Ok(helper)
    }

    pub(super) fn call_continuation_materializer(
        &self,
        builder: &Builder<'ctx>,
        helper: FunctionValue<'ctx>,
        choice_slot: PointerValue<'ctx>,
        target: PointerValue<'ctx>,
        retry: PointerValue<'ctx>,
        value_name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let call = builder.build_call(
            helper,
            &[
                self.frame_nonce.into(),
                self.frame_pointer.into(),
                choice_slot.into(),
                target.into(),
                retry.into(),
            ],
            value_name,
        )?;
        let encoded = call
            .try_as_basic_value()
            .basic()
            .context("VM continuation helper did not return an integer")?
            .into_int_value();
        let lane = builder
            .build_load(self.word, self.frame_pointer, value_name)?
            .into_int_value();
        lane.as_instruction_value()
            .context("VM continuation caller lane load has no instruction")?
            .set_volatile(true)?;
        let encoded_nonce = builder.build_int_add(self.frame_nonce, lane, value_name)?;
        Ok(builder.build_int_sub(encoded, encoded_nonce, value_name)?)
    }

    fn create_domain(
        &self,
        builder: &Builder<'ctx>,
        addresses: &[PointerValue<'ctx>],
        pool_len: u32,
        share_families: &HashMap<usize, ShareFamily>,
        rng: &mut impl Rng,
    ) -> anyhow::Result<TargetDomain<'ctx>> {
        let anchor = *addresses.first().context("empty VM target row")?;
        let mut pool_addresses = Vec::new();
        for address in addresses {
            if !pool_addresses
                .iter()
                .any(|candidate: &PointerValue<'ctx>| candidate.as_value_ref() == address.as_value_ref())
            {
                pool_addresses.push(*address);
            }
        }
        pool_addresses.shuffle(rng);
        let pool_count = u32::try_from(pool_addresses.len())?;
        // Reserve slot zero for a non-target header and leave a random number
        // of additional holes. Each row has its own sparse domain, so a
        // scanner cannot use one global pool/key/anchor invariant.
        let pool_slots = pool_slots(pool_count, pool_len, rng)?;
        let value_name = format!("v{:016x}", rng.random::<u64>());
        let pool = TargetPool {
            pointer: builder.build_alloca(self.word.array_type(pool_len), &value_name)?,
            ty: self.word.array_type(pool_len),
            word: self.word,
            addresses: pool_addresses,
            slots: pool_slots,
            value_name: value_name.clone(),
        };
        let share_pool = TargetPool {
            pointer: builder.build_alloca(self.word.array_type(pool_len), &value_name)?,
            ty: self.word.array_type(pool_len),
            word: self.word,
            addresses: pool.addresses.clone(),
            slots: pool.slots.clone(),
            value_name: value_name.clone(),
        };
        let share_salt = rng.random::<u64>();
        let share_multiplier = rng.random::<u64>() | 1;
        let key = builder.build_alloca(self.word, &value_name)?;
        let anchor_value_slot = builder.build_alloca(self.word, &value_name)?;
        // The key is public but row-specific. It shares the invocation nonce
        // with the state machine without creating a single function-wide key.
        let key_salt = rng.random::<u64>();
        let key_value = builder.build_xor(self.frame_nonce, self.word.const_int(key_salt, false), &value_name)?;
        builder.build_store(key, key_value)?.set_volatile(true)?;
        // Selector tokens live in a row-local integer vault. The resolver
        // call therefore receives a runtime load instead of a literal bucket
        // ordinal; the token is masked with the same invocation-bound key
        // family as the row pool.
        let selector_vault_type = self.word.array_type(pool_len);
        let selector_vault = builder.build_alloca(selector_vault_type, &value_name)?;
        let selector_mask = builder.build_xor(key_value, self.word.const_int(rng.random(), false), &value_name)?;
        for slot in 0..pool_len {
            let cell = builder.build_in_bounds_gep2(
                selector_vault_type,
                selector_vault,
                &[self.word.const_zero(), self.word.const_int(u64::from(slot), false)],
                &value_name,
            )?;
            builder
                .build_store(cell, self.word.const_int(rng.random(), false))?
                .set_volatile(true)?;
        }
        for (slot, address) in pool.slots.iter().zip(&pool.addresses) {
            let resolver = *self
                .target_resolvers
                .get(&(address.as_value_ref() as usize))
                .context("VM target resolver selector is missing")?;
            let masked_selector = builder.build_xor(
                selector_mask,
                self.word.const_int(resolver.selector, false),
                &value_name,
            )?;
            let cell = builder.build_in_bounds_gep2(
                selector_vault_type,
                selector_vault,
                &[self.word.const_zero(), self.word.const_int(u64::from(*slot), false)],
                &value_name,
            )?;
            builder.build_store(cell, masked_selector)?.set_volatile(true)?;
        }
        let anchor_slot = pool
            .slots
            .iter()
            .zip(&pool.addresses)
            .find(|(_, address)| address.as_value_ref() == anchor.as_value_ref())
            .map(|(slot, _)| *slot)
            .context("VM target anchor selector slot is missing")?;
        let anchor_selector_cell = builder.build_in_bounds_gep2(
            selector_vault_type,
            selector_vault,
            &[
                self.word.const_zero(),
                self.word.const_int(u64::from(anchor_slot), false),
            ],
            &value_name,
        )?;
        let masked_anchor_selector = builder
            .build_load2(self.word, anchor_selector_cell, &value_name)?
            .into_int_value();
        masked_anchor_selector
            .as_instruction_value()
            .context("VM anchor selector vault load")?
            .set_volatile(true)?;
        let anchor_selector = builder.build_xor(masked_anchor_selector, selector_mask, &value_name)?;
        let anchor_resolver = *self
            .target_resolvers
            .get(&(anchor.as_value_ref() as usize))
            .context("VM target anchor resolver is missing")?;
        let encoded_anchor_selector = builder.build_xor(self.frame_nonce, anchor_selector, &value_name)?;
        let anchor_value = self.call_target_bucket_materializer(
            builder,
            anchor_resolver.helper,
            encoded_anchor_selector,
            anchor_resolver.bucket,
            anchor_resolver.abi,
            &value_name,
        )?;
        builder
            .build_store(anchor_value_slot, anchor_value)?
            .set_volatile(true)?;
        let pool_anchor = builder
            .build_load2(self.word, anchor_value_slot, &value_name)?
            .into_int_value();
        pool_anchor
            .as_instruction_value()
            .context("VM pool anchor load")?
            .set_volatile(true)?;
        pool.store(
            builder,
            self.word.const_zero(),
            self.word.const_int(rng.random(), false),
        )?;
        share_pool.store(
            builder,
            self.word.const_zero(),
            self.word.const_int(rng.random(), false),
        )?;
        for index in 1..pool_len {
            pool.store(
                builder,
                self.word.const_int(u64::from(index), false),
                self.word.const_int(rng.random(), false),
            )?;
            share_pool.store(
                builder,
                self.word.const_int(u64::from(index), false),
                self.word.const_int(rng.random(), false),
            )?;
        }
        for (slot, address) in pool.slots.iter().zip(&pool.addresses) {
            let share_family = *share_families
                .get(&(address.as_value_ref() as usize))
                .context("VM target share family is missing")?;
            let resolver = *self
                .target_resolvers
                .get(&(address.as_value_ref() as usize))
                .context("VM target resolver is missing")?;
            let selector_cell = builder.build_in_bounds_gep2(
                selector_vault_type,
                selector_vault,
                &[self.word.const_zero(), self.word.const_int(u64::from(*slot), false)],
                &value_name,
            )?;
            let masked_selector = builder
                .build_load2(self.word, selector_cell, &value_name)?
                .into_int_value();
            masked_selector
                .as_instruction_value()
                .context("VM target selector vault load")?
                .set_volatile(true)?;
            let selector = builder.build_xor(masked_selector, selector_mask, &value_name)?;
            let encoded_selector = builder.build_xor(self.frame_nonce, selector, &value_name)?;
            let target_value = self.call_target_bucket_materializer(
                builder,
                resolver.helper,
                encoded_selector,
                resolver.bucket,
                resolver.abi,
                &value_name,
            )?;
            let relative = builder.build_int_sub(target_value, pool_anchor, &value_name)?;
            // Derive the split from the invocation nonce and the physical
            // slot. A static reader must follow the same frame-bound dataflow
            // before it can pair the two pool values; no compile-time share
            // constant is repeated across invocations.
            let share = builder.build_xor(self.frame_nonce, self.word.const_int(share_salt, false), &value_name)?;
            let share = builder.build_int_mul(share, self.word.const_int(share_multiplier, false), &value_name)?;
            let share = builder.build_int_add(share, self.word.const_int(u64::from(*slot), false), &value_name)?;
            let (first, second) = match share_family {
                ShareFamily::XorAdd => {
                    let first = builder.build_xor(share, key_value, &value_name)?;
                    let residual = builder.build_xor(relative, share, &value_name)?;
                    let second = builder.build_int_add(residual, key_value, &value_name)?;
                    (first, second)
                },
                ShareFamily::AddXor => {
                    let first = builder.build_int_add(share, key_value, &value_name)?;
                    let residual = builder.build_int_sub(relative, share, &value_name)?;
                    let second = builder.build_xor(residual, key_value, &value_name)?;
                    (first, second)
                },
            };
            pool.store(builder, self.word.const_int(u64::from(*slot), false), first)?;
            share_pool.store(builder, self.word.const_int(u64::from(*slot), false), second)?;
        }
        Ok(TargetDomain {
            key,
            anchor_value: anchor_value_slot,
            pointer: anchor.get_type(),
            pool,
            share_pool,
        })
    }

    pub(super) fn add_row(
        &mut self,
        builder: &Builder<'ctx>,
        addresses: &[PointerValue<'ctx>],
        gateway_tags: &[u64],
        programs: &[AddressProgram],
        decode_mode: DecodeMode,
        rng: &mut impl Rng,
    ) -> anyhow::Result<()> {
        ensure!(
            addresses.len() == gateway_tags.len(),
            "VM gateway tag count does not match target row"
        );
        ensure!(
            programs.len() == usize::try_from(TARGET_VARIANTS)?,
            "VM decoder variant count does not match target table"
        );
        let shard_count = u32::try_from(addresses.len().min(4).max(1))?;
        // Keep every shard the same size.  Lookup evaluates all shard loads
        // before selecting one, so non-selected loads must remain in-bounds
        // even when their decoded index was produced with another shard key.
        let shard_gap = rng.random_range(1..=shard_count.min(8));
        let pool_len = u32::try_from(
            addresses
                .len()
                .checked_add(usize::try_from(shard_gap)?)
                .and_then(|count| count.checked_add(1))
                .context("VM target shard pool is too large")?,
        )?;
        let mut share_families = HashMap::new();
        for address in addresses {
            share_families
                .entry(address.as_value_ref() as usize)
                .or_insert_with(|| ShareFamily::generate(rng));
        }
        let mut shard_addresses = vec![Vec::new(); usize::try_from(shard_count)?];
        for (index, address) in addresses.iter().enumerate() {
            let shard = index % usize::try_from(shard_count)?;
            if !shard_addresses[shard]
                .iter()
                .any(|candidate: &PointerValue<'ctx>| candidate.as_value_ref() == address.as_value_ref())
            {
                shard_addresses[shard].push(*address);
            }
        }
        let domains = shard_addresses
            .iter()
            .map(|shard| self.create_domain(builder, shard, pool_len, &share_families, rng))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let template = HashLayout::generate(addresses.len(), rng)?;
        let buckets = template.buckets;
        let family = template.family;
        let mut layouts = Vec::with_capacity(usize::try_from(TARGET_VARIANTS)?);
        for _ in 0..TARGET_VARIANTS {
            let mut layout = None;
            for _ in 0..64 {
                let candidate = HashLayout {
                    buckets,
                    salt: rng.random(),
                    multiplier: rng.random::<u64>() | 1,
                    family,
                };
                let slots: HashSet<_> = (0..u32::try_from(addresses.len())?)
                    .map(|index| candidate.slot(index))
                    .collect();
                if slots.len() == addresses.len() {
                    layout = Some(candidate);
                    break;
                }
            }
            layouts.push(layout.context("VM hash variant failed to produce an injective layout")?);
        }
        let runtime_programs = match decode_mode {
            DecodeMode::CandidateSelect => programs.to_vec(),
            DecodeMode::SharedProgram => {
                let shared = programs.first().context("VM decoder programs are empty")?.clone();
                vec![shared; usize::try_from(TARGET_VARIANTS)?]
            },
        };
        let static_programs = (0..TARGET_VARIANTS)
            .map(|_| StaticProgram::generate(rng, self.word.get_bit_width()))
            .collect::<Vec<_>>();
        let static_keys = (0..TARGET_VARIANTS)
            .map(|_| StaticProgram::constant(rng, self.word.get_bit_width()))
            .collect::<Vec<_>>();
        let target_family = TargetFamily::generate(rng, self.word.get_bit_width());
        let selector = VariantSelector::generate(rng, self.word.get_bit_width());
        let variant_layout = VariantLayout::generate(rng);
        let word = self.word;
        let tag_payload_mask = (1u64 << (word.get_bit_width() - 1)) - 1;
        ensure!(
            gateway_tags.iter().all(|tag| *tag & !tag_payload_mask == 0),
            "VM gateway tags must reserve the share-family bit"
        );
        // Slot four is reserved as the invocation-dependent displacement spill
        // slot; the ordinary permutation still addresses physical slots 0..3.
        let record_width = rng.random_range(5..=8);
        let entry = word.array_type(record_width);
        let permutation_entry = word.array_type(4);
        let permutation_ty = permutation_entry.array_type(TARGET_VARIANTS);
        let stride = buckets.checked_add(1).context("VM hash row stride overflow")?;
        let total_slots = stride
            .checked_mul(TARGET_VARIANTS)
            .context("VM hash row variant table overflow")?;
        let ty = entry.array_type(total_slots);
        let records = Records {
            pointer: builder.build_alloca(ty, &self.value_name)?,
            ty,
            permutation_pointer: builder.build_alloca(permutation_ty, &self.value_name)?,
            permutation_ty,
            word,
            frame_nonce: self.frame_nonce,
            row_salt: rng.random(),
            variant_salt: rng.random::<u64>() | 1,
            permutation_salt: rng.random(),
            value_name: self.value_name.clone(),
        };
        records.initialize_permutations(builder)?;
        // Materialize only queried cells in the invocation-local frame. Volatile
        // accesses keep the envelope removal separate from its constant stores;
        // all constants remain public, but there is no static record array.
        for variant in 0..TARGET_VARIANTS {
            let variant_index = usize::try_from(variant)?;
            let logical_variant = word.const_int(u64::from(variant), false);
            let layout = &layouts[variant_index];
            let offset = variant_layout.physical(variant) * stride;
            let header = word.const_int(u64::from(offset), false);
            records.store(
                builder,
                header,
                0,
                logical_variant,
                word.const_int(layout.multiplier, false),
            )?;
            records.store(builder, header, 1, logical_variant, word.const_int(layout.salt, false))?;
            records.store(
                builder,
                header,
                2,
                logical_variant,
                word.const_int(static_keys[variant_index], false),
            )?;
            for (index, address) in addresses.iter().enumerate() {
                let index = u32::try_from(index)?;
                let tag = word.const_int(layout.tag(index), false);
                let shard = usize::try_from(index % shard_count)?;
                let relative = word.const_int(domains[shard].pool_index(*address)?, false);
                let mixed = relative.const_add(word.const_int(static_keys[variant_index], false));
                let encoded = static_programs[variant_index].encode_constant(mixed);
                let slot = offset
                    .checked_add(layout.slot(index))
                    .context("VM hash slot overflow")?;
                let slot = word.const_int(u64::from(slot), false);
                records.store(builder, slot, 0, logical_variant, tag)?;
                records.store(builder, slot, 1, logical_variant, encoded)?;
                records.store(
                    builder,
                    slot,
                    3,
                    logical_variant,
                    word.const_int(
                        share_families
                            .get(&(addresses[usize::try_from(index)?].as_value_ref() as usize))
                            .context("VM target share family is missing")?
                            .pack_tag(gateway_tags[usize::try_from(index)?], word.get_bit_width()),
                        false,
                    ),
                )?;
            }
        }

        let row = u32::try_from(self.rows.len())?;
        for variant in 0..TARGET_VARIANTS {
            let variant_index = usize::try_from(variant)?;
            let layout = &layouts[variant_index];
            let offset = variant_layout.physical(variant) * stride;
            let logical_variant = word.const_int(u64::from(variant), false);
            let static_key = records.load(
                builder,
                word.const_int(u64::from(offset), false),
                2,
                logical_variant,
                &self.value_name,
            )?;
            for index in 0..u32::try_from(addresses.len())? {
                let shard = usize::try_from(index % shard_count)?;
                let slot = word.const_int(u64::from(offset + layout.slot(index)), false);
                let tag = records.load(builder, slot, 0, logical_variant, &self.value_name)?;
                let masked = records.load(builder, slot, 1, logical_variant, &self.value_name)?;
                let mixed = static_programs[variant_index].decode(builder, masked, &self.value_name)?;
                let displacement = builder.build_int_sub(mixed, static_key, &self.value_name)?;
                let base = self.load_key(builder, domains[shard].key)?;
                let key = builder.build_xor(base, tag, &self.value_name)?;
                let encoded = runtime_programs[variant_index].encode(builder, displacement, key, row)?;
                records.store(builder, slot, 1, logical_variant, encoded)?;
            }
        }
        // Only valid generated tokens are queried; padding buckets are never read.
        self.rows.push(HashRow {
            records,
            buckets,
            family,
            selector,
            variant_layout,
            target_family,
            decode_mode,
            programs: runtime_programs,
            domains,
            shard_count,
            pool_len,
        });
        Ok(())
    }

    fn load_key(&self, builder: &Builder<'ctx>, key: PointerValue<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
        let loaded = builder.build_load2(self.word, key, &self.value_name)?;
        loaded
            .as_instruction_value()
            .context("VM key load")?
            .set_volatile(true)?;
        Ok(loaded.into_int_value())
    }

    pub(super) fn lookup(
        &self,
        builder: &Builder<'ctx>,
        row: u32,
        index: IntValue<'ctx>,
        state: IntValue<'ctx>,
    ) -> anyhow::Result<TargetLookup<'ctx>> {
        let data = self.rows.get(usize::try_from(row)?).context("VM hash row is missing")?;
        let word = self.word;
        let index = if index.get_type() == word {
            index
        } else {
            builder.build_int_z_extend(index, word, &self.value_name)?
        };
        let state = if state.get_type() == word {
            state
        } else {
            builder.build_int_z_extend(state, word, &self.value_name)?
        };
        let variant = data.selector.emit(builder, state, &self.value_name)?;
        let stride = word.const_int(u64::from(data.buckets + 1), false);
        let physical_variant = data.variant_layout.emit(builder, variant, &self.value_name)?;
        let variant_offset = builder.build_int_mul(physical_variant, stride, &self.value_name)?;
        let multiplier = data
            .records
            .load(builder, variant_offset, 0, variant, &self.value_name)?;
        let salt = data
            .records
            .load(builder, variant_offset, 1, variant, &self.value_name)?;
        let hash_input = match data.family {
            HashFamily::Affine => index,
            HashFamily::FoldedXorShift { shift } => {
                let shifted = builder.build_right_shift(
                    index,
                    word.const_int(u64::from(shift), false),
                    false,
                    &self.value_name,
                )?;
                builder.build_xor(index, shifted, &self.value_name)?
            },
        };
        let hash = builder.build_int_mul(hash_input, multiplier, &self.value_name)?;
        let hash = builder.build_int_add(hash, salt, &self.value_name)?;
        let bucket = builder.build_and(
            hash,
            word.const_int(u64::from(data.buckets - 1), false),
            &self.value_name,
        )?;
        let slot = builder.build_int_add(
            variant_offset,
            builder.build_int_add(bucket, word.const_int(1, false), &self.value_name)?,
            &self.value_name,
        )?;
        let tag = data.records.load(builder, slot, 0, variant, &self.value_name)?;
        let encoded = data.records.load(builder, slot, 1, variant, &self.value_name)?;
        let shard = if data.shard_count == 1 {
            word.const_zero()
        } else {
            builder.build_int_unsigned_rem(
                index,
                word.const_int(u64::from(data.shard_count), false),
                &self.value_name,
            )?
        };
        let mut selected_key = None;
        for (shard_index, domain) in data.domains.iter().enumerate() {
            let candidate = self.load_key(builder, domain.key)?;
            let matches = builder.build_int_compare(
                IntPredicate::EQ,
                shard,
                word.const_int(u64::try_from(shard_index)?, false),
                &self.value_name,
            )?;
            selected_key = Some(match selected_key {
                None => candidate,
                Some(selected) => builder
                    .build_select(matches, candidate, selected, &self.value_name)?
                    .into_int_value(),
            });
        }
        let selected_key = selected_key.context("VM target shards are empty")?;
        let key = builder.build_xor(selected_key, tag, &self.value_name)?;
        let pool_index = match data.decode_mode {
            DecodeMode::CandidateSelect => {
                let mut pool_index = None;
                for (variant_index, program) in data.programs.iter().enumerate() {
                    let candidate = program.decode(builder, encoded, key, row)?;
                    pool_index = Some(match pool_index {
                        None => candidate,
                        Some(selected) => {
                            let matches = builder.build_int_compare(
                                IntPredicate::EQ,
                                variant,
                                word.const_int(u64::try_from(variant_index)?, false),
                                &self.value_name,
                            )?;
                            builder
                                .build_select(matches, candidate, selected, &self.value_name)?
                                .into_int_value()
                        },
                    });
                }
                pool_index.context("VM candidate decoder variants are empty")?
            },
            DecodeMode::SharedProgram => data
                .programs
                .first()
                .context("VM shared decoder program is empty")?
                .decode(builder, encoded, key, row)?,
        };
        // Candidate decoder variants can produce arbitrary values for
        // non-selected shards.  Normalize every candidate before its load so
        // evaluating all branch-free candidates remains defined.
        let safe_pool_index = builder.build_int_unsigned_rem(
            pool_index,
            word.const_int(u64::from(data.pool_len), false),
            &self.value_name,
        )?;
        let mut keyed_share = None;
        let mut keyed_residual = None;
        let mut anchor_value = None;
        for (shard_index, domain) in data.domains.iter().enumerate() {
            let candidate_share = domain.pool.load(builder, safe_pool_index)?;
            let candidate_residual = domain.share_pool.load(builder, safe_pool_index)?;
            let candidate_anchor = builder.build_load2(word, domain.anchor_value, &self.value_name)?;
            candidate_anchor
                .as_instruction_value()
                .context("VM anchor load")?
                .set_volatile(true)?;
            let candidate_anchor = candidate_anchor.into_int_value();
            let matches = builder.build_int_compare(
                IntPredicate::EQ,
                shard,
                word.const_int(u64::try_from(shard_index)?, false),
                &self.value_name,
            )?;
            keyed_share = Some(match keyed_share {
                None => candidate_share,
                Some(selected) => builder
                    .build_select(matches, candidate_share, selected, &self.value_name)?
                    .into_int_value(),
            });
            keyed_residual = Some(match keyed_residual {
                None => candidate_residual,
                Some(selected) => builder
                    .build_select(matches, candidate_residual, selected, &self.value_name)?
                    .into_int_value(),
            });
            anchor_value = Some(match anchor_value {
                None => candidate_anchor,
                Some(selected) => builder
                    .build_select(matches, candidate_anchor, selected, &self.value_name)?
                    .into_int_value(),
            });
        }
        let keyed_share = keyed_share.context("VM target shard pools are empty")?;
        let keyed_residual = keyed_residual.context("VM target shard share pools are empty")?;
        // The gateway tag is retrieved from the same row-local record and
        // hash slot as the target displacement. This removes the old linear
        // index-to-tag select chain from the source transfer block.
        let packed_gateway_tag = data.records.load(builder, slot, 3, variant, &self.value_name)?;
        let family_bit = builder.build_right_shift(
            packed_gateway_tag,
            word.const_int(u64::from(word.get_bit_width() - 1), false),
            false,
            &self.value_name,
        )?;
        let use_add_xor =
            builder.build_int_compare(IntPredicate::NE, family_bit, word.const_zero(), &self.value_name)?;
        let tag_mask = (1u64 << (word.get_bit_width() - 1)) - 1;
        let gateway_tag = builder.build_and(packed_gateway_tag, word.const_int(tag_mask, false), &self.value_name)?;
        let xor_share = builder.build_xor(keyed_share, selected_key, &self.value_name)?;
        let xor_residual = builder.build_int_sub(keyed_residual, selected_key, &self.value_name)?;
        let xor_displacement = builder.build_xor(xor_share, xor_residual, &self.value_name)?;
        let add_share = builder.build_int_sub(keyed_share, selected_key, &self.value_name)?;
        let add_residual = builder.build_xor(keyed_residual, selected_key, &self.value_name)?;
        let add_displacement = builder.build_int_add(add_share, add_residual, &self.value_name)?;
        let displacement = builder
            .build_select(use_add_xor, add_displacement, xor_displacement, &self.value_name)?
            .into_int_value();
        let anchor = anchor_value.context("VM target shard anchors are empty")?;
        let address = match data.target_family {
            TargetFamily::Direct => builder.build_int_add(displacement, anchor, &self.value_name)?,
            TargetFamily::Reverse => {
                let negated = builder.build_int_sub(word.const_zero(), displacement, &self.value_name)?;
                builder.build_int_sub(anchor, negated, &self.value_name)?
            },
            TargetFamily::Rekeyed => {
                // Separate volatile reads keep the reconstruction dependent on
                // invocation-local memory instead of exposing one foldable
                // arithmetic expression.
                let mask_a = selected_key;
                let mask_b = selected_key;
                let mixed = builder.build_xor(displacement, mask_a, &self.value_name)?;
                let mixed = builder.build_int_add(mixed, mask_b, &self.value_name)?;
                let mixed = builder.build_int_sub(mixed, mask_b, &self.value_name)?;
                let restored = builder.build_xor(mixed, mask_a, &self.value_name)?;
                builder.build_int_add(anchor, restored, &self.value_name)?
            },
            TargetFamily::Rotated { shift } => {
                let left = builder.build_left_shift(
                    displacement,
                    word.const_int(u64::from(shift), false),
                    &self.value_name,
                )?;
                let right = builder.build_right_shift(
                    displacement,
                    word.const_int(u64::from(word.get_bit_width() - shift), false),
                    false,
                    &self.value_name,
                )?;
                let rotated = builder.build_or(left, right, &self.value_name)?;
                let left = builder.build_left_shift(
                    rotated,
                    word.const_int(u64::from(word.get_bit_width() - shift), false),
                    &self.value_name,
                )?;
                let right = builder.build_right_shift(
                    rotated,
                    word.const_int(u64::from(shift), false),
                    false,
                    &self.value_name,
                )?;
                let restored = builder.build_or(left, right, &self.value_name)?;
                builder.build_int_add(anchor, restored, &self.value_name)?
            },
        };
        Ok(TargetLookup {
            pointer: builder.build_int_to_ptr(
                address,
                data.domains.first().context("VM target shards are empty")?.pointer,
                &self.value_name,
            )?,
            gateway_tag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AddressProgram, HashFamily, HashLayout, ShareFamily, TARGET_VARIANTS, VariantLayout, VariantSelector,
        pool_layout,
    };
    use rand::{Rng, SeedableRng, rngs::StdRng};
    use std::collections::HashSet;
    use std::num::NonZeroU32;

    #[test]
    fn hash_layout_is_injective_at_both_pointer_widths() {
        let mut rng = StdRng::seed_from_u64(42);
        for count in [1, 2, 3, 7, 8, 17, 255, 256, 257] {
            for _ in 0..32 {
                let layout = HashLayout::generate(count, &mut rng).unwrap();
                let slots: HashSet<_> = (0..u32::try_from(count).unwrap()).map(|i| layout.slot(i)).collect();
                assert_eq!(slots.len(), count);
                assert!(slots.iter().all(|slot| (1..=layout.buckets).contains(slot)));
                assert!(usize::try_from(layout.buckets).unwrap() >= count * 2);
                for index in 0..u32::try_from(count).unwrap() {
                    let mul32 = u32::try_from(layout.multiplier & u64::from(u32::MAX)).unwrap();
                    let salt32 = u32::try_from(layout.salt & u64::from(u32::MAX)).unwrap();
                    let hash_input = match layout.family {
                        HashFamily::Affine => index,
                        HashFamily::FoldedXorShift { shift } => index ^ (index >> shift),
                    };
                    assert_eq!(
                        layout.slot(index),
                        1 + (hash_input.wrapping_mul(mul32).wrapping_add(salt32) & (layout.buckets - 1))
                    );
                }
            }
        }
        let all_ones = HashLayout {
            buckets: 512,
            salt: u64::MAX,
            multiplier: u64::MAX,
            family: HashFamily::Affine,
        };
        assert_eq!((0..512).map(|i| all_ones.slot(i)).collect::<HashSet<_>>().len(), 512);
        for shift in [1, 3, 7, 15, 31] {
            let folded = HashLayout {
                buckets: 1024,
                salt: 0x1234_5678,
                multiplier: 0x9e37_79b9,
                family: HashFamily::FoldedXorShift { shift },
            };
            assert_eq!((0..257).map(|i| folded.slot(i)).collect::<HashSet<_>>().len(), 257);
        }
        assert!(HashLayout::generate(0, &mut rng).is_err());
        assert!(HashLayout::generate(usize::MAX, &mut rng).is_err());
        for _ in 0..100 {
            let layout = HashLayout::generate(16, &mut rng).unwrap();
            let input: u32 = rng.random_range(0..16);
            assert_eq!(layout.tag(input) ^ layout.salt, u64::from(input));
        }
    }

    #[test]
    fn resolver_selector_preimages_round_trip_at_both_pointer_widths() {
        let mut rng = StdRng::seed_from_u64(0x51ec_70u64);
        for bits in [32, 64] {
            for _ in 0..128 {
                let program = AddressProgram::generate(&mut rng, bits);
                let values = [0, 1, 2, 3, u64::MAX, rng.random()];
                for value in values {
                    let value = if bits == 32 { value & u64::from(u32::MAX) } else { value };
                    let token = program.invert_constant(value, bits);
                    assert_eq!(program.apply_constant(token, bits), value);
                }
            }
        }
    }

    #[test]
    fn sparse_pool_layout_keeps_header_holes_and_unique_slots() {
        let mut rng = StdRng::seed_from_u64(0x50a5);
        let mut saw_more_than_one_shape = false;
        let mut previous = None;
        for count in [1, 2, 3, 8, 17, 64] {
            for _ in 0..64 {
                let (pool_len, slots) = pool_layout(count, &mut rng).unwrap();
                assert_eq!(slots.len(), usize::try_from(count).unwrap());
                assert!(slots.iter().all(|slot| (1..pool_len).contains(slot)));
                assert_eq!(slots.iter().copied().collect::<HashSet<_>>().len(), slots.len());
                assert!(pool_len > count + 1);
                if previous.is_some_and(|length| length != pool_len) {
                    saw_more_than_one_shape = true;
                }
                previous = Some(pool_len);
            }
        }
        assert!(saw_more_than_one_shape);
    }

    #[test]
    fn variant_selectors_cover_families_and_two_bit_domain() {
        let mut rng = StdRng::seed_from_u64(0x5eed);
        for bits in [32, 64] {
            let mut kinds = HashSet::new();
            for _ in 0..512 {
                let selector = VariantSelector::generate(&mut rng, bits);
                kinds.insert(selector.kind());
                for value in [0, 1, u64::from(u32::MAX), 0x8000_0000, u64::MAX, rng.random()] {
                    assert!(selector.apply(value, bits) < TARGET_VARIANTS);
                }
            }
            assert_eq!(kinds, [0, 1, 2, 3].into_iter().collect());
        }
    }

    #[test]
    fn variant_layouts_are_permutations_of_the_four_rows() {
        let mut rng = StdRng::seed_from_u64(0x1a70_0u64);
        for _ in 0..512 {
            let layout = VariantLayout::generate(&mut rng);
            let positions: HashSet<_> = (0..TARGET_VARIANTS).map(|logical| layout.physical(logical)).collect();
            assert_eq!(positions, (0..TARGET_VARIANTS).collect());
        }
    }

    #[test]
    fn target_share_families_round_trip_at_both_pointer_widths() {
        let mut rng = StdRng::seed_from_u64(0x5a7e);
        for bits in [32, 64] {
            let mask = if bits == 32 { u64::from(u32::MAX) } else { u64::MAX };
            for family in [ShareFamily::XorAdd, ShareFamily::AddXor] {
                for _ in 0..512 {
                    let relative = rng.random::<u64>() & mask;
                    let key = rng.random::<u64>() & mask;
                    let share = rng.random::<u64>() & mask;
                    let (first, second) = match family {
                        ShareFamily::XorAdd => ((share ^ key) & mask, (relative ^ share).wrapping_add(key) & mask),
                        ShareFamily::AddXor => (
                            share.wrapping_add(key) & mask,
                            (relative.wrapping_sub(share) ^ key) & mask,
                        ),
                    };
                    let recovered = match family {
                        ShareFamily::XorAdd => ((first ^ key) ^ second.wrapping_sub(key)) & mask,
                        ShareFamily::AddXor => ((first.wrapping_sub(key)).wrapping_add(second ^ key)) & mask,
                    };
                    assert_eq!(recovered, relative, "family={family:?}, bits={bits}");
                }
            }
        }
    }

    #[test]
    fn target_share_family_tag_envelopes_round_trip_at_both_pointer_widths() {
        for bits in [32, 64] {
            let payload_mask = (1u64 << (bits - 1)) - 1;
            for family in [ShareFamily::XorAdd, ShareFamily::AddXor] {
                for tag in [0, 1, payload_mask / 3, payload_mask] {
                    let packed = family.pack_tag(tag, bits);
                    let (decoded, decoded_family) = ShareFamily::unpack_tag(packed, bits);
                    assert_eq!(decoded, tag);
                    assert_eq!(decoded_family, family);
                }
            }
        }
    }

    #[test]
    fn emitted_record_permutations_are_bijective_at_both_word_widths() {
        use super::Records;
        use amice_llvm::inkwell2::BuilderExt;
        use amice_plugin::inkwell::{OptimizationLevel, context::Context};

        let mut rng = StdRng::seed_from_u64(0xcac4e);
        for bits in [32, 64] {
            for salts in [[0, 1, 0], [u64::MAX, u64::MAX, u64::MAX], rng.random()] {
                let context = Context::create();
                let module = context.create_module("field_permutation");
                let builder = context.create_builder();
                let abi_word = context.i64_type();
                let word = context
                    .custom_width_int_type(NonZeroU32::new(bits).expect("test word width is nonzero"))
                    .expect("test word width is supported");
                let function = module.add_function(
                    "fields",
                    abi_word.fn_type(&[abi_word.into(), abi_word.into()], false),
                    None,
                );
                builder.position_at_end(context.append_basic_block(function, "entry"));
                let nonce = function.get_first_param().unwrap().into_int_value();
                let variant = function.get_nth_param(1).unwrap().into_int_value();
                let (nonce, variant) = if bits == 32 {
                    (
                        builder.build_int_truncate(nonce, word, "nonce").unwrap(),
                        builder.build_int_truncate(variant, word, "variant").unwrap(),
                    )
                } else {
                    (nonce, variant)
                };
                let ty = word.array_type(4).array_type(TARGET_VARIANTS);
                let records = Records {
                    pointer: builder.build_alloca(ty, "records").unwrap(),
                    ty,
                    permutation_pointer: builder.build_alloca(ty, "cache").unwrap(),
                    permutation_ty: ty,
                    word,
                    frame_nonce: nonce,
                    row_salt: salts[0],
                    variant_salt: salts[1],
                    permutation_salt: salts[2],
                    value_name: "field".to_owned(),
                };
                records.initialize_permutations(&builder).unwrap();
                let mut packed = abi_word.const_zero();
                for field in 0..4u64 {
                    let cell = builder
                        .build_in_bounds_gep2(
                            ty,
                            records.permutation_pointer,
                            &[word.const_zero(), variant, word.const_int(field, false)],
                            "cell",
                        )
                        .unwrap();
                    let value = builder.build_load2(word, cell, "position").unwrap().into_int_value();
                    let value = if bits == 32 {
                        builder.build_int_z_extend(value, abi_word, "wide").unwrap()
                    } else {
                        value
                    };
                    let shifted = builder
                        .build_left_shift(value, abi_word.const_int(field * 8, false), "shifted")
                        .unwrap();
                    packed = builder.build_or(packed, shifted, "packed").unwrap();
                }
                builder.build_return(Some(&packed)).unwrap();
                module.verify().unwrap();
                let engine = module.create_jit_execution_engine(OptimizationLevel::None).unwrap();
                // SAFETY: the verified function above has exactly this integer
                // C ABI; both the module and JIT engine outlive all calls.
                let fields = unsafe { engine.get_function::<unsafe extern "C" fn(u64, u64) -> u64>("fields") }.unwrap();
                let nonces = [0, 1, 7, 8, 16, 0xffff_ffff, 0x8000_0000_0000_0000, u64::MAX]
                    .into_iter()
                    .chain((0..128).map(|_| rng.random()));
                for nonce in nonces {
                    for variant in 0..u64::from(TARGET_VARIANTS) {
                        // SAFETY: the signature was checked above and every
                        // variant is within the initialized cache bounds.
                        let packed = unsafe { fields.call(nonce, variant) };
                        let positions: HashSet<_> = (0..4).map(|field| (packed >> (field * 8)) & 0xff).collect();
                        assert_eq!(
                            positions,
                            (0..4).collect(),
                            "bits={bits} nonce={nonce} variant={variant}"
                        );
                        assert_eq!(packed >> 32, 0);
                    }
                }
            }
        }
    }
}

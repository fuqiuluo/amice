use amice_plugin::inkwell::{builder::Builder, values::IntValue};
use rand::{Rng, seq::SliceRandom};

/// A public, per-function reversible nonlinear network.
///
/// This deliberately is not a cryptographic key or a cryptographic S-box. It
/// replaces the much easier-to-extract 256-byte lookup table with a small
/// composition of bijections over `Z/(2^32)`. The constants are generated per
/// function, and the same network is emitted inline at every VM transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Add(u32),
    Sub(u32),
    Xor(u32),
    Multiply(u32),
    RotateLeft(u32),
    XorShiftRight(u32),
}

/// A seed-dependent table-free permutation used by the index programs.
pub(super) struct SBox {
    operations: Vec<Op>,
    value_name: String,
}

impl SBox {
    pub(super) fn generate(rng: &mut impl Rng) -> Self {
        let count = rng.random_range(4..=7);
        let value_name = format!("v{:016x}", rng.random::<u64>());
        let mut operations = vec![
            // Keep at least one additive and one multiplicative operation in
            // every generated network. The latter is the nonlinear part over
            // the word ring; all multipliers are odd and therefore invertible.
            Op::Add(rng.random_range(1..=u32::MAX)),
            Op::Multiply(rng.random::<u32>() | 1),
        ];
        while operations.len() < count {
            operations.push(match rng.random_range(0..4) {
                0 => Op::Add(rng.random_range(1..=u32::MAX)),
                1 => Op::Sub(rng.random_range(1..=u32::MAX)),
                2 => Op::Xor(rng.random_range(1..=u32::MAX)),
                _ => {
                    if rng.random_bool(0.5) {
                        Op::RotateLeft(rng.random_range(1..32))
                    } else {
                        Op::XorShiftRight(rng.random_range(1..32))
                    }
                },
            });
        }
        operations.shuffle(rng);
        Self { operations, value_name }
    }

    fn inverse_op(op: Op, value: u32) -> u32 {
        match op {
            Op::Add(k) => value.wrapping_sub(k),
            Op::Sub(k) => value.wrapping_add(k),
            Op::Xor(k) => value ^ k,
            Op::Multiply(k) => {
                // Newton iteration doubles the number of correct bits each
                // round for an odd multiplier modulo 2^32.
                let mut inverse = 1u32;
                for _ in 0..5 {
                    inverse = inverse.wrapping_mul(2u32.wrapping_sub(k.wrapping_mul(inverse)));
                }
                value.wrapping_mul(inverse)
            },
            Op::RotateLeft(k) => value.rotate_right(k),
            Op::XorShiftRight(k) => {
                let mut result = value;
                let mut shift = k;
                while shift < 32 {
                    result ^= value >> shift;
                    shift += k;
                }
                result
            },
        }
    }

    pub(super) fn invert(&self, value: u32) -> u32 {
        self.operations
            .iter()
            .rev()
            .fold(value, |value, op| Self::inverse_op(*op, value))
    }

    fn keyed<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        key: IntValue<'ctx>,
        constant: u32,
        operation: usize,
        odd: bool,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let tweak = word.const_int(u64::from(u32::try_from(operation)?.wrapping_mul(0x9e37_79b9)), false);
        let mixed_key = builder.build_int_add(key, tweak, name)?;
        let mixed_key = if odd {
            builder.build_and(mixed_key, word.const_int(u64::from(u32::MAX - 1), false), name)?
        } else {
            mixed_key
        };
        let constant = if odd { constant | 1 } else { constant };
        Ok(builder.build_xor(word.const_int(u64::from(constant), false), mixed_key, name)?)
    }

    fn dynamic_rotate<'ctx>(
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        key: IntValue<'ctx>,
        operation: usize,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let tweak = word.const_int(u64::from(u32::try_from(operation)?.wrapping_mul(0x9e37_79b9)), false);
        let mixed = builder.build_int_add(key, tweak, name)?;
        // Keep the shift in 1..31.  A 32-bit shift is poison in LLVM IR.
        let masked = builder.build_and(mixed, word.const_int(30, false), name)?;
        Ok(builder.build_int_add(masked, word.const_int(1, false), name)?)
    }

    pub(super) fn emit<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        input: IntValue<'ctx>,
        key: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        self.operations
            .iter()
            .enumerate()
            .try_fold(input, |value, (operation, op)| {
                let constant = |k| value.get_type().const_int(u64::from(k), false);
                Ok(match *op {
                    Op::Add(k) => builder.build_int_add(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Sub(k) => builder.build_int_sub(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Xor(k) => builder.build_xor(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Multiply(k) => builder.build_int_mul(
                        value,
                        Self::keyed(builder, value, key, k, operation, true, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::RotateLeft(_k) => {
                        let shift = Self::dynamic_rotate(builder, value, key, operation, &self.value_name)?;
                        let opposite = builder.build_int_sub(constant(32), shift, &self.value_name)?;
                        let left = builder.build_left_shift(value, shift, &self.value_name)?;
                        let right = builder.build_right_shift(value, opposite, false, &self.value_name)?;
                        builder.build_or(left, right, &self.value_name)?
                    },
                    Op::XorShiftRight(k) => {
                        // The shift stays static because its inverse needs a
                        // compile-time unrolled sequence; arithmetic constants are
                        // still keyed by the runtime state.
                        let shifted = builder.build_right_shift(value, constant(k), false, &self.value_name)?;
                        builder.build_xor(value, shifted, &self.value_name)?
                    },
                })
            })
    }

    /// Emit the inverse permutation used when the source transfer constructs
    /// its runtime token. Keeping this counterpart in IR removes the static
    /// pre-encoded successor constants from the source block.
    pub(super) fn emit_inverse<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        input: IntValue<'ctx>,
        key: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        self.operations
            .iter()
            .enumerate()
            .rev()
            .try_fold(input, |value, (operation, op)| {
                let constant = |k| value.get_type().const_int(u64::from(k), false);
                Ok(match *op {
                    Op::Add(k) => builder.build_int_sub(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Sub(k) => builder.build_int_add(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Xor(k) => builder.build_xor(
                        value,
                        Self::keyed(builder, value, key, k, operation, false, &self.value_name)?,
                        &self.value_name,
                    )?,
                    Op::Multiply(k) => {
                        let multiplier = Self::keyed(builder, value, key, k, operation, true, &self.value_name)?;
                        let mut reciprocal = value.get_type().const_int(1, false);
                        for _ in 0..5 {
                            let product = builder.build_int_mul(multiplier, reciprocal, &self.value_name)?;
                            let correction = builder.build_int_sub(
                                value.get_type().const_int(2, false),
                                product,
                                &self.value_name,
                            )?;
                            reciprocal = builder.build_int_mul(reciprocal, correction, &self.value_name)?;
                        }
                        builder.build_int_mul(value, reciprocal, &self.value_name)?
                    },
                    Op::RotateLeft(_k) => {
                        let shift = Self::dynamic_rotate(builder, value, key, operation, &self.value_name)?;
                        let opposite = builder.build_int_sub(constant(32), shift, &self.value_name)?;
                        let left = builder.build_left_shift(value, opposite, &self.value_name)?;
                        let right = builder.build_right_shift(value, shift, false, &self.value_name)?;
                        builder.build_or(left, right, &self.value_name)?
                    },
                    Op::XorShiftRight(k) => {
                        let mut result = value;
                        let mut shift = k;
                        while shift < 32 {
                            let shifted = builder.build_right_shift(value, constant(shift), false, &self.value_name)?;
                            result = builder.build_xor(result, shifted, &self.value_name)?;
                            shift += k;
                        }
                        result
                    },
                })
            })
    }

    #[cfg(test)]
    pub(super) fn apply(&self, value: u32) -> u32 {
        self.operations.iter().fold(value, |value, op| match *op {
            Op::Add(k) => value.wrapping_add(k),
            Op::Sub(k) => value.wrapping_sub(k),
            Op::Xor(k) => value ^ k,
            Op::Multiply(k) => value.wrapping_mul(k),
            Op::RotateLeft(k) => value.rotate_left(k),
            Op::XorShiftRight(k) => value ^ (value >> k),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};

    #[test]
    fn generated_networks_round_trip_word_boundaries() {
        for seed in 0..64 {
            let sbox = SBox::generate(&mut StdRng::seed_from_u64(seed));
            for value in [0, 1, 0xff, 0x1234_abcd, 0x8000_0000, u32::MAX] {
                assert_eq!(sbox.apply(sbox.invert(value)), value);
                assert_eq!(sbox.invert(sbox.apply(value)), value);
            }
        }
    }

    #[test]
    fn generated_networks_are_not_shared_by_default() {
        let mut rng = StdRng::seed_from_u64(0);
        let first = SBox::generate(&mut rng);
        let second = SBox::generate(&mut rng);
        assert_ne!(first.operations, second.operations);
    }
}

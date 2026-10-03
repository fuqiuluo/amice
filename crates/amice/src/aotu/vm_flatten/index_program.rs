use super::sbox::SBox;
use amice_plugin::inkwell::{IntPredicate, builder::Builder, values::IntValue};
use anyhow::ensure;
use rand::Rng;

/// Bijections on Z/(2^32). Each transfer gets its own composition and constants.
#[derive(Clone, Copy, Debug)]
enum Op {
    Add(u32),
    Sub(u32),
    Xor(u32),
    Multiply(u32),
    RotateLeft(u32),
    XorShiftRight(u32),
    Substitute,
}

impl Op {
    fn inverse(self, value: u32, sbox: &SBox) -> u32 {
        match self {
            Self::Substitute => sbox.invert(value),
            Self::Add(k) => value.wrapping_sub(k),
            Self::Sub(k) => value.wrapping_add(k),
            Self::Xor(k) => value ^ k,
            Self::Multiply(k) => {
                // An odd multiplier has an inverse modulo 2^32. Each Newton
                // step doubles the number of correct bits, starting with one.
                let mut inverse = 1u32;
                for _ in 0..5 {
                    inverse = inverse.wrapping_mul(2u32.wrapping_sub(k.wrapping_mul(inverse)));
                }
                value.wrapping_mul(inverse)
            },
            Self::RotateLeft(k) => value.rotate_right(k),
            Self::XorShiftRight(k) => {
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

    fn emit<'ctx>(
        self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        sbox: &SBox,
        key: IntValue<'ctx>,
        operation: usize,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let tweak = word.const_int(u64::from(u32::try_from(operation)?.wrapping_mul(0x9e37_79b9)), false);
        let keyed = |constant: u32, odd: bool| -> anyhow::Result<IntValue<'ctx>> {
            let mixed = builder.build_int_add(key, tweak, name)?;
            let mixed = if odd {
                builder.build_and(mixed, word.const_int(u64::from(u32::MAX - 1), false), name)?
            } else {
                mixed
            };
            let constant = if odd { constant | 1 } else { constant };
            Ok(builder.build_xor(word.const_int(u64::from(constant), false), mixed, name)?)
        };
        Ok(match self {
            Self::Substitute => sbox.emit(builder, value, key)?,
            Self::Add(k) => builder.build_int_add(value, keyed(k, false)?, name)?,
            Self::Sub(k) => builder.build_int_sub(value, keyed(k, false)?, name)?,
            Self::Xor(k) => builder.build_xor(value, keyed(k, false)?, name)?,
            Self::Multiply(k) => builder.build_int_mul(value, keyed(k, true)?, name)?,
            Self::RotateLeft(_k) => {
                let mixed = builder.build_int_add(key, tweak, name)?;
                let shift = builder.build_int_add(
                    // Keep the shift in 1..31; shifting by the word width is poison.
                    builder.build_and(mixed, word.const_int(30, false), name)?,
                    word.const_int(1, false),
                    name,
                )?;
                let opposite = builder.build_int_sub(word.const_int(32, false), shift, name)?;
                let left = builder.build_left_shift(value, shift, name)?;
                let right = builder.build_right_shift(value, opposite, false, name)?;
                builder.build_or(left, right, name)?
            },
            Self::XorShiftRight(k) => {
                let shifted = builder.build_right_shift(value, word.const_int(u64::from(k), false), false, name)?;
                builder.build_xor(value, shifted, name)?
            },
        })
    }

    fn emit_inverse<'ctx>(
        self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        sbox: &SBox,
        key: IntValue<'ctx>,
        operation: usize,
        name: &str,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let word = value.get_type();
        let tweak = word.const_int(u64::from(u32::try_from(operation)?.wrapping_mul(0x9e37_79b9)), false);
        let keyed = |constant: u32, odd: bool| -> anyhow::Result<IntValue<'ctx>> {
            let mixed = builder.build_int_add(key, tweak, name)?;
            let mixed = if odd {
                builder.build_and(mixed, word.const_int(u64::from(u32::MAX - 1), false), name)?
            } else {
                mixed
            };
            let constant = if odd { constant | 1 } else { constant };
            Ok(builder.build_xor(word.const_int(u64::from(constant), false), mixed, name)?)
        };
        Ok(match self {
            Self::Substitute => sbox.emit_inverse(builder, value, key)?,
            Self::Add(k) => builder.build_int_sub(value, keyed(k, false)?, name)?,
            Self::Sub(k) => builder.build_int_add(value, keyed(k, false)?, name)?,
            Self::Xor(k) => builder.build_xor(value, keyed(k, false)?, name)?,
            Self::Multiply(k) => {
                let multiplier = keyed(k, true)?;
                let mut reciprocal = word.const_int(1, false);
                for _ in 0..5 {
                    let product = builder.build_int_mul(multiplier, reciprocal, name)?;
                    let correction = builder.build_int_sub(word.const_int(2, false), product, name)?;
                    reciprocal = builder.build_int_mul(reciprocal, correction, name)?;
                }
                builder.build_int_mul(value, reciprocal, name)?
            },
            Self::RotateLeft(_k) => {
                let mixed = builder.build_int_add(key, tweak, name)?;
                let shift = builder.build_int_add(
                    // Keep the shift in 1..31; shifting by the word width is poison.
                    builder.build_and(mixed, word.const_int(30, false), name)?,
                    word.const_int(1, false),
                    name,
                )?;
                let opposite = builder.build_int_sub(word.const_int(32, false), shift, name)?;
                let left = builder.build_left_shift(value, opposite, name)?;
                let right = builder.build_right_shift(value, shift, false, name)?;
                builder.build_or(left, right, name)?
            },
            Self::XorShiftRight(k) => {
                let mut result = value;
                let mut shift = k;
                while shift < 32 {
                    let shifted =
                        builder.build_right_shift(value, word.const_int(u64::from(shift), false), false, name)?;
                    result = builder.build_xor(result, shifted, name)?;
                    shift += k;
                }
                result
            },
        })
    }
}

pub(super) struct IndexProgram {
    operations: Vec<Op>,
    value_name: String,
}

impl IndexProgram {
    pub(super) fn generate(rng: &mut impl Rng, max_ops: u32) -> Self {
        let count = rng.random_range(1..=max_ops.clamp(1, 32));
        Self::generate_with_count(rng, count)
    }

    pub(super) fn generate_variants<const N: usize>(rng: &mut impl Rng, max_ops: u32) -> [Self; N] {
        let count = rng.random_range(1..=max_ops.clamp(1, 32));
        std::array::from_fn(|_| Self::generate_with_count(rng, count))
    }

    fn generate_with_count(rng: &mut impl Rng, count: u32) -> Self {
        let substitution = rng.random_range(0..count);
        Self {
            operations: (0..count)
                .map(|index| {
                    if index == substitution {
                        Op::Substitute
                    } else {
                        match rng.random_range(0..6) {
                            0 => Op::Add(rng.random_range(1..=u32::MAX)),
                            1 => Op::Sub(rng.random_range(1..=u32::MAX)),
                            2 => Op::Xor(rng.random_range(1..=u32::MAX)),
                            3 => Op::Multiply(rng.random::<u32>() | 3),
                            4 => Op::RotateLeft(rng.random_range(1..32)),
                            _ => Op::XorShiftRight(rng.random_range(1..32)),
                        }
                    }
                })
                .collect(),
            value_name: format!("v{:016x}", rng.random::<u64>()),
        }
    }

    pub(super) fn encode(&self, index: u32, sbox: &SBox) -> u32 {
        self.operations
            .iter()
            .rev()
            .fold(index, |value, op| op.inverse(value, sbox))
    }

    fn emit_operation<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        substitutions: &[SBox; 3],
        selector: IntValue<'ctx>,
        key: IntValue<'ctx>,
        operation: usize,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let op = self.operations[operation];
        if matches!(op, Op::Substitute) {
            let mut selected = op.emit(builder, value, &substitutions[0], key, operation, &self.value_name)?;
            for (index, substitution) in substitutions.iter().enumerate().skip(1) {
                let candidate = op.emit(builder, value, substitution, key, operation, &self.value_name)?;
                let matches = builder.build_int_compare(
                    IntPredicate::EQ,
                    selector,
                    value.get_type().const_int(u64::try_from(index)?, false),
                    &self.value_name,
                )?;
                selected = builder
                    .build_select(matches, candidate, selected, &self.value_name)?
                    .into_int_value();
            }
            Ok(selected)
        } else {
            op.emit(builder, value, &substitutions[0], key, operation, &self.value_name)
        }
    }

    fn emit_inverse_operation<'ctx>(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        substitutions: &[SBox; 3],
        selector: IntValue<'ctx>,
        key: IntValue<'ctx>,
        operation: usize,
    ) -> anyhow::Result<IntValue<'ctx>> {
        let op = self.operations[operation];
        if matches!(op, Op::Substitute) {
            let mut selected = substitutions[0].emit_inverse(builder, value, key)?;
            for (index, substitution) in substitutions.iter().enumerate().skip(1) {
                let candidate = substitution.emit_inverse(builder, value, key)?;
                let matches = builder.build_int_compare(
                    IntPredicate::EQ,
                    selector,
                    value.get_type().const_int(u64::try_from(index)?, false),
                    &self.value_name,
                )?;
                selected = builder
                    .build_select(matches, candidate, selected, &self.value_name)?
                    .into_int_value();
            }
            Ok(selected)
        } else {
            Ok(op.emit_inverse(builder, value, &substitutions[0], key, operation, &self.value_name)?)
        }
    }

    /// Build one shared selector schedule for the whole program. The inverse
    /// emitter walks operations in reverse, so selectors are materialized in
    /// original operation order and indexed by that order in both directions.
    /// This keeps the source-side inverse and decoder-side forward networks
    /// semantically paired while making the selected candidate vary at every
    /// operation.
    fn emit_selector_schedule<'ctx, const N: usize>(
        builder: &Builder<'ctx>,
        key: IntValue<'ctx>,
        operation_count: usize,
        name: &str,
    ) -> anyhow::Result<Vec<(IntValue<'ctx>, IntValue<'ctx>)>> {
        ensure!(N > 0, "VM opcode program variants cannot be empty");
        let word = key.get_type();
        let mut schedule = key;
        let mut selectors = Vec::with_capacity(operation_count);
        for operation in 0..operation_count {
            let operation = u64::try_from(operation)?;
            let tweak = word.const_int(operation.wrapping_mul(0x9e37_79b9), false);
            schedule = builder.build_xor(schedule, tweak, name)?;
            schedule = builder.build_int_mul(schedule, word.const_int(0x9e37_79b1, false), name)?;
            schedule = builder.build_int_add(schedule, word.const_int(0x7f4a_7c15, false), name)?;
            let substitution_selector = builder.build_and(schedule, word.const_int(3, false), name)?;
            let variant_selector =
                builder.build_int_unsigned_rem(schedule, word.const_int(u64::try_from(N)?, false), name)?;
            selectors.push((substitution_selector, variant_selector));
        }
        Ok(selectors)
    }

    pub(super) fn emit_variants<'ctx, const N: usize>(
        programs: &[Self; N],
        builder: &Builder<'ctx>,
        input: IntValue<'ctx>,
        substitutions: &[[SBox; 3]; N],
        key: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        ensure!(N > 0, "VM opcode program variants cannot be empty");
        ensure!(
            programs
                .windows(2)
                .all(|pair| pair[0].operations.len() == pair[1].operations.len()),
            "VM opcode program variants must have equal operation counts"
        );
        let selectors =
            Self::emit_selector_schedule::<N>(builder, key, programs[0].operations.len(), &programs[0].value_name)?;
        programs[0]
            .operations
            .iter()
            .enumerate()
            .try_fold(input, |value, (operation, _)| {
                let (selector, variant_selector) = selectors[operation];
                let mut selected =
                    programs[0].emit_operation(builder, value, &substitutions[0], selector, key, operation)?;
                for (variant, (program, substitution)) in programs.iter().zip(substitutions).enumerate().skip(1) {
                    let candidate = program.emit_operation(builder, value, substitution, selector, key, operation)?;
                    let matches = builder.build_int_compare(
                        IntPredicate::EQ,
                        variant_selector,
                        value.get_type().const_int(u64::try_from(variant)?, false),
                        &programs[0].value_name,
                    )?;
                    selected = builder
                        .build_select(matches, candidate, selected, &programs[0].value_name)?
                        .into_int_value();
                }
                Ok(selected)
            })
    }

    pub(super) fn emit_inverse_variants<'ctx, const N: usize>(
        programs: &[Self; N],
        builder: &Builder<'ctx>,
        input: IntValue<'ctx>,
        substitutions: &[[SBox; 3]; N],
        key: IntValue<'ctx>,
    ) -> anyhow::Result<IntValue<'ctx>> {
        ensure!(N > 0, "VM opcode program variants cannot be empty");
        ensure!(
            programs
                .windows(2)
                .all(|pair| pair[0].operations.len() == pair[1].operations.len()),
            "VM opcode program variants must have equal operation counts"
        );
        let selectors =
            Self::emit_selector_schedule::<N>(builder, key, programs[0].operations.len(), &programs[0].value_name)?;
        programs[0]
            .operations
            .iter()
            .enumerate()
            .rev()
            .try_fold(input, |value, (operation, _)| {
                let (selector, variant_selector) = selectors[operation];
                let mut selected =
                    programs[0].emit_inverse_operation(builder, value, &substitutions[0], selector, key, operation)?;
                for (variant, (program, substitution)) in programs.iter().zip(substitutions).enumerate().skip(1) {
                    let candidate =
                        program.emit_inverse_operation(builder, value, substitution, selector, key, operation)?;
                    let matches = builder.build_int_compare(
                        IntPredicate::EQ,
                        variant_selector,
                        value.get_type().const_int(u64::try_from(variant)?, false),
                        &programs[0].value_name,
                    )?;
                    selected = builder
                        .build_select(matches, candidate, selected, &programs[0].value_name)?
                        .into_int_value();
                }
                Ok(selected)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};

    fn evaluate(program: &IndexProgram, input: u32, sbox: &SBox) -> u32 {
        program.operations.iter().fold(input, |v, op| match *op {
            Op::Substitute => sbox.apply(v),
            Op::Add(k) => v.wrapping_add(k),
            Op::Sub(k) => v.wrapping_sub(k),
            Op::Xor(k) => v ^ k,
            Op::Multiply(k) => v.wrapping_mul(k),
            Op::RotateLeft(k) => v.rotate_left(k),
            Op::XorShiftRight(k) => v ^ (v >> k),
        })
    }

    #[test]
    fn index_preimages_cover_boundary_values_and_random_compositions() {
        let mut rng = StdRng::seed_from_u64(0);
        let sbox = SBox::generate(&mut rng);
        for max_ops in [0, 1, 8, 32, u32::MAX] {
            for _ in 0..256 {
                let program = IndexProgram::generate(&mut rng, max_ops);
                assert!((1..=max_ops.clamp(1, 32) as usize).contains(&program.operations.len()));
                assert_eq!(
                    program
                        .operations
                        .iter()
                        .filter(|op| matches!(op, Op::Substitute))
                        .count(),
                    1
                );
                for index in [0, 1, 31, 255, 65535, 0x8000_0000, u32::MAX, rng.random()] {
                    assert_eq!(evaluate(&program, program.encode(index, &sbox), &sbox), index);
                }
            }
        }
        for shift in 1..32 {
            let program = IndexProgram {
                operations: vec![Op::XorShiftRight(shift)],
                value_name: "test".to_owned(),
            };
            for index in [0, 1, u32::MAX, 0x8000_0000, 0xaaaa_5555] {
                assert_eq!(evaluate(&program, program.encode(index, &sbox), &sbox), index);
            }
        }
    }

    #[test]
    fn state_selected_preimages_round_trip_through_three_networks() {
        let mut rng = StdRng::seed_from_u64(0x5eed);
        let substitutions = [
            SBox::generate(&mut rng),
            SBox::generate(&mut rng),
            SBox::generate(&mut rng),
        ];
        for _ in 0..256 {
            let program = IndexProgram::generate(&mut rng, 32);
            for index in [0, 1, 31, 255, 65535, 0x8000_0000, u32::MAX, rng.random()] {
                for selector in [0u32, 1, 2] {
                    let encoded = program.encode(index, &substitutions[selector as usize]);
                    let decoded = evaluate(&program, encoded, &substitutions[selector as usize]);
                    assert_eq!(decoded, index, "selector={selector:#x}");
                }
            }
        }
    }

    #[test]
    fn generated_variant_programs_share_length_and_have_one_substitute() {
        let mut rng = StdRng::seed_from_u64(0xdec0_deed);
        for max_ops in [0, 1, 8, 32] {
            for _ in 0..64 {
                let variants = IndexProgram::generate_variants::<3>(&mut rng, max_ops);
                let length = variants[0].operations.len();
                assert!(variants.iter().all(|program| program.operations.len() == length));
                assert!(variants.iter().all(|program| {
                    program
                        .operations
                        .iter()
                        .filter(|op| matches!(op, Op::Substitute))
                        .count()
                        == 1
                }));
            }
        }
    }
}

//! Integer identities over Z/(2^w), lowered immediately to carrier operations.
//! References 0 and 1 are the frozen inputs; each step appends one result.
use amice_plugin::inkwell::values::InstructionOpcode as Op;

#[derive(Clone, Copy)]
pub(super) struct Step(pub Op, pub usize, pub usize);

pub(super) fn recipe(op: Op, variant: bool) -> &'static [Step] {
    use Op::*;
    match (op, variant) {
        // x+y = (x^y) + (x&y) + (x&y) = 2*(x|y) - (x^y)
        (Add, false) => &[Step(Xor, 0, 1), Step(And, 0, 1), Step(Add, 2, 3), Step(Add, 4, 3)],
        (Add, true) => &[Step(Or, 0, 1), Step(Xor, 0, 1), Step(Add, 2, 2), Step(Sub, 4, 3)],
        // (x|y)-x = ~x&y; (x|y)-y = x&~y, including modular overflow.
        (Sub, false) => &[
            Step(Or, 0, 1),
            Step(Sub, 2, 0),
            Step(Xor, 0, 1),
            Step(Sub, 4, 3),
            Step(Sub, 5, 3),
        ],
        (Sub, true) => &[
            Step(Or, 0, 1),
            Step(Sub, 2, 1),
            Step(Xor, 0, 1),
            Step(Add, 3, 3),
            Step(Sub, 5, 4),
        ],
        (And, false) => &[Step(Or, 0, 1), Step(Xor, 0, 1), Step(Sub, 2, 3)],
        (And, true) => &[Step(Add, 0, 1), Step(Or, 0, 1), Step(Sub, 2, 3)],
        (Or, false) => &[Step(Add, 0, 1), Step(And, 0, 1), Step(Sub, 2, 3)],
        (Or, true) => &[Step(Xor, 0, 1), Step(And, 0, 1), Step(Add, 2, 3)],
        (Xor, false) => &[Step(Add, 0, 1), Step(And, 0, 1), Step(Sub, 2, 3), Step(Sub, 4, 3)],
        (Xor, true) => &[Step(Or, 0, 1), Step(And, 0, 1), Step(Sub, 2, 3)],
        _ => unreachable!("unsupported MBA identity"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identity_and_variant_is_exhaustive_through_i8() {
        // Both variants must be checked; a random compiler seed cannot provide
        // deterministic coverage of all generator identities.
        for width in 1..=8 {
            let mask = (1u32 << width) - 1;
            for op in [Op::Add, Op::Sub, Op::And, Op::Or, Op::Xor] {
                for variant in [false, true] {
                    for x in 0..=mask {
                        for y in 0..=mask {
                            let apply = |op, a: u32, b: u32| match op {
                                Op::Add => a.wrapping_add(b) & mask,
                                Op::Sub => a.wrapping_sub(b) & mask,
                                Op::And => a & b,
                                Op::Or => a | b,
                                Op::Xor => a ^ b,
                                _ => unreachable!(),
                            };
                            let mut values = vec![x, y];
                            for &Step(op, a, b) in recipe(op, variant) {
                                values.push(apply(op, values[a], values[b]));
                            }
                            assert_eq!(*values.last().unwrap(), apply(op, x, y));
                        }
                    }
                }
            }
        }
    }
}

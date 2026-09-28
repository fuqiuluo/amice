use crate::config::BogusControlFlowConfig;
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, InstructionExt, VerifyResult};
use amice_plugin::inkwell::{
    IntPredicate,
    attributes::{Attribute, AttributeLoc},
    builder::{Builder, BuilderError},
    llvm_sys::core::{LLVMBuildFreeze, LLVMGetNumSuccessors, LLVMGetSuccessor},
    module::{Linkage, Module},
    types::IntType,
    values::{AsValueRef, BasicValue, FunctionValue, InstructionOpcode as Op, InstructionValue, IntValue},
};
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::collections::{HashMap, HashSet};

fn operand(inst: InstructionValue<'_>, index: u32) -> IntValue<'_> {
    inst.get_operand(index)
        .and_then(|operand| operand.value())
        .expect("selected integer binary instructions have two value operands")
        .into_int_value()
}

fn eligible(inst: InstructionValue<'_>) -> bool {
    // Check the opcode before converting: EH tokens are not basic values.
    matches!(inst.get_opcode(), Op::Add | Op::Sub | Op::And | Op::Or | Op::Xor)
        && IntValue::try_from(inst).is_ok_and(|v| matches!(v.get_type().get_bit_width(), 8 | 16 | 32 | 64))
}

/// Mark reachable blocks and all cyclic SCCs, including irreducible loops.
/// Both DFS walks are iterative so large functions do not consume call stack.
fn classify_cfg(edges: &[Vec<usize>]) -> (Vec<bool>, Vec<bool>) {
    let mut reachable = vec![false; edges.len()];
    let mut cyclic = vec![false; edges.len()];
    if edges.is_empty() {
        return (reachable, cyclic);
    }
    let mut reverse = vec![Vec::new(); edges.len()];
    for (from, successors) in edges.iter().enumerate() {
        for &to in successors {
            reverse[to].push(from);
        }
    }
    let mut order = Vec::new();
    let mut stack = vec![(0, 0)];
    reachable[0] = true;
    while let Some((node, next)) = stack.last_mut() {
        if *next < edges[*node].len() {
            let to = edges[*node][*next];
            *next += 1;
            if !reachable[to] {
                reachable[to] = true;
                stack.push((to, 0));
            }
        } else {
            order.push(*node);
            stack.pop();
        }
    }
    let mut assigned = vec![false; edges.len()];
    for &root in order.iter().rev() {
        if assigned[root] {
            continue;
        }
        let mut component = Vec::new();
        let mut pending = vec![root];
        assigned[root] = true;
        while let Some(node) = pending.pop() {
            component.push(node);
            for &from in &reverse[node] {
                if reachable[from] && !assigned[from] {
                    assigned[from] = true;
                    pending.push(from);
                }
            }
        }
        if component.len() > 1 || edges[root].contains(&root) {
            for node in component {
                cyclic[node] = true;
            }
        }
    }
    (reachable, cyclic)
}

/// Rewrite connected integer regions in a verified function.
///
/// # Errors
/// Returns an IR builder error if staging a replacement fails.
///
/// # Panics
/// Panics if a selected region violates the planner's invariants or LLVM
/// produces invalid IR after a committed rewrite, which cannot be rolled back.
pub(super) fn lower_to_digit_loops<'ctx>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    config: &BogusControlFlowConfig,
    seed: u64,
) -> anyhow::Result<u32> {
    if function.count_basic_blocks() == 0
        || function.has_personality_function()
        || super::has_entry_block_intrinsics(function)
        || function
            .get_enum_attribute(AttributeLoc::Function, Attribute::get_named_enum_kind_id("naked"))
            .is_some()
        || ["amice.bcf.generated", "amice.bcf.done"]
            .iter()
            .any(|key| function.get_string_attribute(AttributeLoc::Function, key).is_some())
        || config.probability == 0
        || config.max_regions == 0
        || config.max_region_instructions < 2
    {
        return Ok(0);
    }
    let blocks = function.get_basic_blocks();
    if blocks.iter().any(|b| b.has_address_taken() || b.is_eh_pad()) || !function.verify(true) {
        return Ok(0);
    }
    let indices: HashMap<_, _> = blocks.iter().enumerate().map(|(i, b)| (b.as_mut_ptr(), i)).collect();
    let edges: Vec<Vec<usize>> = blocks
        .iter()
        .map(|b| {
            let term = b
                .get_terminator()
                .expect("verified blocks have a terminator")
                .as_value_ref();
            // SAFETY: Verification guarantees a live terminator and successors
            // belonging to this function; planning does not mutate the CFG.
            unsafe {
                (0..LLVMGetNumSuccessors(term))
                    .map(|i| indices[&LLVMGetSuccessor(term, i)])
                    .collect()
            }
        })
        .collect();
    let (reachable, cyclic) = classify_cfg(&edges);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut plans = Vec::new();
    let max_regions = usize::try_from(config.max_regions.min(16)).expect("a limit of 16 fits usize");
    let max_instructions = usize::try_from(config.max_region_instructions.min(16)).expect("a limit of 16 fits usize");
    let probability = u64::from(config.probability.min(100));
    let mut flush = |run: &mut Vec<InstructionValue<'ctx>>| {
        // Each instruction after the first was required to consume a member.
        if run.len() >= 2 && plans.len() < max_regions && rng.random::<u64>() % 100 < probability {
            plans.push(std::mem::take(run));
        }
        run.clear();
    };
    for (index, block) in blocks.iter().enumerate() {
        if !reachable[index] || cyclic[index] {
            continue;
        }
        let mut run: Vec<InstructionValue<'ctx>> = Vec::new();
        for inst in block.get_instructions() {
            if !eligible(inst) {
                flush(&mut run);
                continue;
            }
            if !run.is_empty() && (run[0].get_type() != inst.get_type() || run.len() >= max_instructions) {
                flush(&mut run);
            }
            if !run.is_empty() && !(0..2).any(|i| operand(inst, i).as_instruction().is_some_and(|v| run.contains(&v))) {
                flush(&mut run);
            }
            run.push(inst);
        }
        flush(&mut run);
    }
    let mut count = 0;
    // Snapshot originals and rewrite consumers first. Earlier RAUW operations
    // then update their live-ins; generated instructions are never reconsidered.
    for region in plans.iter().rev() {
        count += u32::from(lower_region_to_digit_loop(module, function, region, &mut rng)?);
    }
    if count != 0 {
        function.add_attribute(
            AttributeLoc::Function,
            module.get_context().create_string_attribute("amice.bcf.done", "1"),
        );
    }
    Ok(count)
}

fn unsigned_cast<'ctx>(b: &Builder<'ctx>, v: IntValue<'ctx>, ty: IntType<'ctx>) -> anyhow::Result<IntValue<'ctx>> {
    Ok(b.build_int_cast_sign_flag(v, ty, false, "")?)
}

struct RegionHelper<'ctx> {
    function: FunctionValue<'ctx>,
    inputs: Vec<IntValue<'ctx>>,
}

impl<'ctx> RegionHelper<'ctx> {
    fn build(
        module: &Module<'ctx>,
        region: &[InstructionValue<'ctx>],
        multiplier: u32,
        salt: u32,
    ) -> anyhow::Result<Option<Self>> {
        let ctx = module.get_context();
        let ty = IntValue::try_from(region[0])
            .expect("regions contain only integer instructions")
            .get_type();
        let width = ty.get_bit_width();
        let members: HashSet<_> = region
            .iter()
            .map(|i| IntValue::try_from(*i).expect("regions contain only integer instructions"))
            .collect();
        let mut inputs = Vec::with_capacity(region.len() * 2);
        for &inst in region {
            for i in 0..2 {
                let value = operand(inst, i);
                if !members.contains(&value) && !inputs.contains(&value) {
                    inputs.push(value);
                }
            }
        }
        if inputs.iter().all(|v| v.is_const()) {
            return Ok(None);
        }
        let result_ty = ctx.struct_type(&vec![ty.into(); region.len()], false);
        let helper = module.add_function(
            ".amice.bcf.region",
            result_ty.fn_type(&vec![ty.into(); inputs.len()], false),
            Some(Linkage::Internal),
        );
        helper.add_attribute(
            AttributeLoc::Function,
            ctx.create_string_attribute("amice.bcf.generated", "1"),
        );
        for name in ["nounwind", "willreturn"] {
            helper.add_attribute(
                AttributeLoc::Function,
                ctx.create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0),
            );
        }
        let memory_kind = Attribute::get_named_enum_kind_id("memory");
        helper.add_attribute(
            AttributeLoc::Function,
            ctx.create_enum_attribute(
                if memory_kind != 0 {
                    memory_kind
                } else {
                    Attribute::get_named_enum_kind_id("readnone")
                },
                0,
            ),
        );
        let built = (|| -> anyhow::Result<()> {
            let entry = ctx.append_basic_block(helper, "entry");
            let digits_block = ctx.append_basic_block(helper, "bcf.digits");
            let exit = ctx.append_basic_block(helper, "bcf.exit");
            let b = ctx.create_builder();
            b.position_at_end(entry);
            let i32_ty = ctx.i32_type();
            let constant = |n: u32| i32_ty.const_int(u64::from(n), false);
            let mut boundary = HashMap::with_capacity(inputs.len());
            let mut seed = constant(salt);
            for (&input, arg) in inputs.iter().zip(helper.get_param_iter()) {
                let arg = arg.into_int_value();
                boundary.insert(input, arg);
                seed = b.build_xor(seed, unsigned_cast(&b, arg, i32_ty)?, "")?;
            }
            b.build_unconditional_branch(digits_block)?;
            b.position_at_end(digits_block);
            let position = b.build_phi(i32_ty, "bcf.position")?;
            let state = b.build_phi(i32_ty, "bcf.state")?;
            position.add_incoming(&[(&constant(0), entry)]);
            state.add_incoming(&[(&seed, entry)]);
            let mut accumulators = Vec::with_capacity(region.len());
            let mut carries = Vec::with_capacity(region.len());
            for inst in region {
                let acc = b.build_phi(ty, "bcf.acc")?;
                acc.add_incoming(&[(&ty.const_zero(), entry)]);
                accumulators.push(acc);
                carries.push(if matches!(inst.get_opcode(), Op::Add | Op::Sub) {
                    let carry = b.build_phi(i32_ty, "bcf.carry")?;
                    carry.add_incoming(&[(&constant(0), entry)]);
                    Some(carry)
                } else {
                    None
                });
            }
            let position_value = position.as_basic_value().into_int_value();
            let state_value = state.as_basic_value().into_int_value();
            // Position stays below width and is a multiple of four. Clamp the
            // final half-byte to four bits, bounding execution by width / 4 steps.
            let remaining = b.build_int_sub(constant(width), position_value, "")?;
            let high_bit = b.build_and(state_value, constant(0x8000_0000), "")?;
            let small = b.build_or(
                b.build_int_compare(IntPredicate::NE, high_bit, constant(0), "")?,
                b.build_int_compare(IntPredicate::ULT, remaining, constant(8), "")?,
                "",
            )?;
            let step = b
                .build_select(small, constant(4), constant(8), "bcf.step")?
                .into_int_value();
            let mask = b
                .build_select(small, constant(15), constant(255), "bcf.mask")?
                .into_int_value();
            let word_position = unsigned_cast(&b, position_value, ty)?;
            let mut digits = HashMap::with_capacity(inputs.len() + region.len());
            let mut next_acc = Vec::with_capacity(region.len());
            for (i, &inst) in region.iter().enumerate() {
                let mut digit = |v| -> anyhow::Result<IntValue<'ctx>> {
                    if let Some(&d) = digits.get(&v) {
                        return Ok(d);
                    }
                    let shifted = b.build_right_shift(boundary[&v], word_position, false, "")?;
                    let d = b.build_and(unsigned_cast(&b, shifted, i32_ty)?, mask, "")?;
                    digits.insert(v, d);
                    Ok(d)
                };
                let x = digit(operand(inst, 0))?;
                let y = digit(operand(inst, 1))?;
                let d = match inst.get_opcode() {
                    Op::Add | Op::Sub => {
                        let carry = carries[i].expect("add and sub instructions have a carry PHI");
                        let carry_value = carry.as_basic_value().into_int_value();
                        let value = if inst.get_opcode() == Op::Add {
                            b.build_int_add(b.build_int_add(x, y, "")?, carry_value, "")?
                        } else {
                            // 0 <= base + x - y - borrow < 2*base, including i8.
                            let base = b.build_int_add(mask, constant(1), "")?;
                            b.build_int_sub(b.build_int_sub(b.build_int_add(x, base, "")?, y, "")?, carry_value, "")?
                        };
                        let next = b.build_right_shift(value, step, false, "")?;
                        let next = if inst.get_opcode() == Op::Sub {
                            b.build_xor(next, constant(1), "")?
                        } else {
                            next
                        };
                        carry.add_incoming(&[(&next, digits_block)]);
                        b.build_and(value, mask, "")?
                    },
                    Op::And => b.build_and(x, y, "")?,
                    Op::Or => b.build_or(x, y, "")?,
                    Op::Xor => b.build_xor(x, y, "")?,
                    _ => unreachable!("region contains only supported binary instructions"),
                };
                digits.insert(
                    IntValue::try_from(inst).expect("regions contain only integer instructions"),
                    d,
                );
                let acc = b.build_or(
                    accumulators[i].as_basic_value().into_int_value(),
                    b.build_left_shift(unsigned_cast(&b, d, ty)?, word_position, "")?,
                    "",
                )?;
                accumulators[i].add_incoming(&[(&acc, digits_block)]);
                next_acc.push(acc);
            }
            // Odd multiplier makes the affine update reversible for a fixed digit;
            // either schedule computes the same result without an opaque predicate.
            let last = *region.last().expect("the planner selects at least two instructions");
            let feedback = digits[&IntValue::try_from(last).expect("regions contain only integer instructions")];
            let next_state = b.build_int_add(
                b.build_int_add(
                    b.build_int_mul(state_value, constant(multiplier), "")?,
                    constant(salt),
                    "",
                )?,
                feedback,
                "",
            )?;
            state.add_incoming(&[(&next_state, digits_block)]);
            let next_position = b.build_int_add(position_value, step, "")?;
            position.add_incoming(&[(&next_position, digits_block)]);
            b.build_conditional_branch(
                b.build_int_compare(IntPredicate::ULT, next_position, constant(width), "")?,
                digits_block,
                exit,
            )?;
            b.position_at_end(exit);
            let mut result = result_ty.get_undef();
            for (i, acc) in next_acc.iter().enumerate() {
                let index = u32::try_from(i).expect("regions contain at most 16 instructions");
                result = b.build_insert_value(result, *acc, index, "")?.into_struct_value();
            }
            b.build_return(Some(&result))?;
            Ok(())
        })();
        if let Err(err) = built {
            // SAFETY: The helper has no callers yet; all references to its body
            // remain inside the helper and are discarded with it.
            unsafe { helper.delete() };
            return Err(err);
        }
        if !helper.verify(true) {
            // SAFETY: Verification happened before any caller referenced the helper.
            unsafe { helper.delete() };
            return Ok(None);
        }
        Ok(Some(Self {
            function: helper,
            inputs,
        }))
    }
}

fn lower_region_to_digit_loop<'ctx>(
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    region: &[InstructionValue<'ctx>],
    rng: &mut StdRng,
) -> anyhow::Result<bool> {
    let multiplier = rng.random::<u32>() | 1;
    let salt = rng.random::<u32>();
    let Some(RegionHelper {
        function: helper,
        inputs,
    }) = RegionHelper::build(module, region, multiplier, salt)?
    else {
        return Ok(false);
    };
    let b = module.get_context().create_builder();
    b.position_before(&region[0]);
    if let Some(location) = region[0].get_debug_location() {
        b.set_current_debug_location(location);
    }
    let mut added = Vec::with_capacity(inputs.len() + 1 + region.len());
    let staged = (|| -> anyhow::Result<_> {
        let mut args = Vec::with_capacity(inputs.len());
        for input in inputs {
            // Freeze each live-in once: new control flow must not consume poison
            // or independently sample undef. No original overflow flags survive.
            // SAFETY: The builder is positioned before a live instruction;
            // each live-in is an integer in the same LLVM context, so freeze
            // returns an integer of the same type.
            let frozen = unsafe {
                IntValue::new(LLVMBuildFreeze(
                    b.as_mut_ptr(),
                    input.as_value_ref(),
                    c"bcf.input".as_ptr(),
                ))
            };
            if frozen != input
                && let Some(inst) = frozen.as_instruction()
            {
                added.push(inst);
            }
            args.push(frozen.into());
        }
        let call = b.build_call(helper, &args, "bcf.results")?;
        let results = call
            .try_as_basic_value()
            .basic()
            .expect("the helper returns a result struct")
            .into_struct_value();
        let call_inst = results.as_instruction_value().expect("a call is an instruction");
        added.push(call_inst);
        let outputs = (0..region.len())
            .map(|i| {
                let index = u32::try_from(i).expect("regions contain at most 16 instructions");
                let value = b.build_extract_value(results, index, "bcf.value")?;
                let inst = value
                    .as_instruction_value()
                    .expect("extracting a field from a call result emits an instruction");
                added.push(inst);
                Ok(inst)
            })
            .collect::<Result<Vec<_>, BuilderError>>()?;
        Ok((call_inst, outputs))
    })();
    // Original instructions and their uses still exist, so staging can abort.
    if staged.is_err() || !function.verify(true) {
        for inst in added.iter().rev() {
            inst.erase_from_basic_block();
        }
        // SAFETY: Removing staged instructions in reverse use order also
        // removes the only call to this helper. Originals remain untouched.
        unsafe { helper.delete() };
        return staged.map(|_| false);
    }
    let (call, outputs) = staged?;
    for (old, new) in region.iter().zip(outputs.iter()) {
        old.replace_all_uses_with(new);
    }
    for inst in region.iter().rev() {
        inst.erase_from_basic_block();
    }
    // The utility owns SSA/PHI repair, not BCF policy. If inlining is declined,
    // the verified internal helper remains a valid implementation.
    // SAFETY: The call and callee were verified. No handle to the call or its
    // old block layout is used after inlining.
    unsafe { call.into_call_inst().inline() };
    if helper.as_global_value().as_pointer_value().get_first_use().is_none() {
        // SAFETY: Inlining removed the helper's only caller and no use remains.
        unsafe { helper.delete() };
    }
    if let VerifyResult::Broken(reason) = function.verify_function() {
        // The originals have been committed; never let the pass runner swallow
        // this invariant failure and continue optimization with invalid IR.
        panic!("BCF produced invalid IR after inlining: {reason}");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::classify_cfg;

    #[test]
    fn cfg_separates_cycles_from_joins_exits_and_unreachable_blocks() {
        // Diamond 0 -> {1,2} -> 3, irreducible 4 <-> 5 with two entries,
        // exit 6, self-loop 7, and unreachable 8 -> 3.
        let edges = vec![
            vec![1, 2],
            vec![3],
            vec![3],
            vec![4, 5],
            vec![5],
            vec![4, 6, 7],
            vec![],
            vec![7],
            vec![3],
        ];
        let (reachable, cyclic) = classify_cfg(&edges);
        assert_eq!(reachable, vec![true, true, true, true, true, true, true, true, false]);
        assert_eq!(cyclic, vec![false, false, false, false, true, true, false, true, false]);
        assert_eq!(classify_cfg(&[]), (vec![], vec![]));
    }
}

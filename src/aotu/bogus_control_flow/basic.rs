use crate::aotu::bogus_control_flow::BogusControlFlowAlgo;
use crate::config::BogusControlFlowConfig;
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, InstructionExt};
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::llvm_sys::core::{LLVMBuildLoad2, LLVMSetVolatile};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::AsTypeRef;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, GlobalValue, InstructionOpcode, IntValue, PointerValue};
use log::{error, warn};
use rand::Rng;

#[derive(Default)]
pub struct BogusControlFlowBasic {
    x: u32,
    y: u32,
}

impl BogusControlFlowAlgo for BogusControlFlowBasic {
    fn initialize(&mut self, _cfg: &BogusControlFlowConfig, _module: &mut Module<'_>) -> anyhow::Result<()> {
        self.x = rand::random_range(0..2782812982);
        self.y = rand::random_range(0..459846238);

        Ok(())
    }

    fn apply_bogus_control_flow(
        &mut self,
        cfg: &BogusControlFlowConfig,
        module: &mut Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()> {
        // Create global variables for opaque predicates
        let globals = create_opaque_predicate_globals(self, module);

        for _ in 0..cfg.loop_count {
            if let Err(e) = handle_function(self, function, &globals, cfg.probability) {
                warn!("(BogusControlFlow) failed to obfuscate function: {}", e);
            }
        }

        Ok(())
    }
}

/// Create global variables used for opaque predicates
fn create_opaque_predicate_globals<'a>(
    algo: &BogusControlFlowBasic,
    module: &mut Module<'a>,
) -> OpaquePredicateGlobals<'a> {
    let context = module.get_context();
    let i32_type = context.i32_type();

    // Create global x and y variables for opaque predicates
    // These must NOT be constant so optimizer cannot eliminate the loads
    let x_global = module.add_global(i32_type, None, "");
    x_global.set_initializer(&i32_type.const_int(algo.x as u64, false));
    x_global.set_constant(false); // Important: not constant!

    let y_global = module.add_global(i32_type, None, "");
    y_global.set_initializer(&i32_type.const_int(algo.y as u64, false));
    y_global.set_constant(false); // Important: not constant!

    OpaquePredicateGlobals {
        x: x_global,
        y: y_global,
    }
}

struct OpaquePredicateGlobals<'a> {
    x: GlobalValue<'a>,
    y: GlobalValue<'a>,
}

fn handle_function(
    algo: &BogusControlFlowBasic,
    function: FunctionValue<'_>,
    globals: &OpaquePredicateGlobals<'_>,
    probability: u32,
) -> anyhow::Result<()> {
    let basic_blocks = function.get_basic_blocks();

    // Skip if no basic blocks or only entry block
    if basic_blocks.len() <= 1 {
        return Ok(());
    }

    let mut rng = rand::rng();
    let mut blocks_to_modify = Vec::new();
    let entry_block = function.get_entry_block();

    // Collect blocks to modify (excluding entry block)
    for bb in basic_blocks.iter() {
        let random_value: u32 = rng.random();
        if (random_value % 100) < probability {
            blocks_to_modify.push(*bb);
        }
    }

    let Some(entry_block) = entry_block else {
        error!("(bogus-control-flow) entry block not found");
        return Err(anyhow::anyhow!("Entry block not found"));
    };
    blocks_to_modify.retain(|bb| *bb != entry_block);

    let stack_predicates = {
        let context = function.get_type().get_context();
        let i32_type = context.i32_type();
        let builder = context.create_builder();

        let first_insertion_point = entry_block.get_first_insertion_pt();
        builder.position_before(&first_insertion_point);

        let x = builder.build_alloca(i32_type, ".x")?;
        let y = builder.build_alloca(i32_type, ".y")?;
        builder.build_store(x, i32_type.const_int(algo.x as u64, false))?;
        builder.build_store(y, i32_type.const_int(algo.y as u64, false))?;
        (x, y)
    };

    // Apply bogus control flow to selected blocks
    for bb in blocks_to_modify {
        if let Err(e) = apply_bogus_control_flow_to_unconditional_branch(algo, function, bb, globals, &stack_predicates)
        {
            warn!("Failed to apply bogus control flow to block: {}", e);
        }
    }

    Ok(())
}

/// Apply a simplified version of bogus control flow
fn apply_bogus_control_flow_to_unconditional_branch(
    algo: &BogusControlFlowBasic,
    function: FunctionValue<'_>,
    original_block: BasicBlock<'_>,
    globals: &OpaquePredicateGlobals<'_>,
    stack: &(PointerValue, PointerValue),
) -> anyhow::Result<()> {
    // Get terminator instruction before we modify anything
    let terminator = original_block
        .get_terminator()
        .ok_or_else(|| anyhow::anyhow!("Block has no terminator"))?;

    // Only handle simple unconditional branches for now
    if terminator.get_num_operands() != 1 {
        return Ok(()); // Skip complex control flow
    }

    if terminator.get_opcode() != InstructionOpcode::Br {
        return Ok(()); // Skip if not a branch instruction
    }

    let target_bb = terminator
        .into_branch_inst()
        .get_successor(0)
        .ok_or_else(|| anyhow::anyhow!("Cannot get branch target"))?;

    let context = function.get_type().get_context();
    let builder = context.create_builder();

    // Create new blocks
    let condition_block = context.append_basic_block(function, "bogus_cond");
    let fake_block = context.append_basic_block(function, "bogus_fake");

    // Build from original block to condition block
    // 这里因为我在后面删除了终结指令
    // 所以说不会出现因为在终结指令后面新增代码出现校验不通过的问题
    builder.position_at_end(original_block);
    builder.build_unconditional_branch(condition_block)?;
    // target_block可能有来自original_block的
    target_bb.fix_phi_node(original_block, condition_block);

    // Build condition block with opaque predicate
    builder.position_at_end(condition_block);
    let (is_true, condition) = if rand::random::<bool>() {
        create_simple_opaque_predicate(
            algo,
            &builder,
            globals.x.as_pointer_value(),
            globals.y.as_pointer_value(),
        )?
    } else {
        create_simple_opaque_predicate(algo, &builder, stack.0, stack.1)?
    };
    // is_true是true意味着，condition永远为true, then_block -> target_bb
    // is_true是false意味着，condition永远为false, then_block -> fake_block
    let then_block = if is_true { target_bb } else { fake_block };
    let else_block = if is_true { fake_block } else { target_bb };
    builder.build_conditional_branch(condition, then_block, else_block)?;

    // Build fake block (should never be executed - must be unreachable)
    builder.position_at_end(fake_block);
    builder.build_unreachable()?;

    // Do NOT fix PHI nodes to point to fake_block since it's unreachable
    // The only real predecessor of target_bb is condition_block

    terminator.erase_from_basic_block();

    Ok(())
}

/// Volatile-load an i32 from a pointer.
fn volatile_load_i32<'a>(builder: &Builder<'a>, ptr: PointerValue<'a>, name: &std::ffi::CStr) -> IntValue<'a> {
    unsafe {
        let i32_type = builder.get_insert_block().unwrap().get_context().i32_type();
        let load_inst = LLVMBuildLoad2(
            builder.as_mut_ptr() as _,
            i32_type.as_type_ref() as _,
            ptr.as_value_ref() as _,
            name.as_ptr(),
        );
        LLVMSetVolatile(load_inst, 1);
        IntValue::new(load_inst as _)
    }
}

/// Create an opaque predicate based on number-theoretic / bitwise identities
/// that hold for ALL values of x and y, making them resistant to algebraic simplification.
///
/// Returns `(is_always_true, condition)` where `is_always_true` indicates the
/// actual truth value so the caller can wire branches correctly.
fn create_simple_opaque_predicate<'a>(
    _algo: &BogusControlFlowBasic,
    builder: &Builder<'a>,
    x_ptr: PointerValue<'a>,
    y_ptr: PointerValue<'a>,
) -> anyhow::Result<(bool, IntValue<'a>)> {
    let context = builder.get_insert_block().unwrap().get_context();
    let i32_type = context.i32_type();

    let x_val = volatile_load_i32(builder, x_ptr, c"x_val");
    let y_val = volatile_load_i32(builder, y_ptr, c"y_val");

    // Randomly swap x and y for variety
    let (x, y) = if rand::random::<bool>() {
        (x_val, y_val)
    } else {
        (y_val, x_val)
    };

    let mut rng = rand::rng();
    let identity = rng.random_range(0u32..7);
    let negate = rand::random::<bool>();

    let zero = i32_type.const_zero();
    let all_ones = i32_type.const_all_ones();

    // Build an always-true condition from one of 7 identity families.
    // If `negate` is set, we invert the comparison to get always-false.
    let condition = match identity {
        // Identity 1: (x & ~x) == 0  — always true
        0 => {
            let not_x = builder.build_not(x, "not_x")?;
            let and = builder.build_and(x, not_x, "x_and_not_x")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, and, zero, "id_and_compl")?
        }
        // Identity 2: (x | ~x) == -1  — always true
        1 => {
            let not_x = builder.build_not(x, "not_x")?;
            let or = builder.build_or(x, not_x, "x_or_not_x")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, or, all_ones, "id_or_compl")?
        }
        // Identity 3: (x ^ y) | (x & y) == (x | y)  — always true
        2 => {
            let xor = builder.build_xor(x, y, "x_xor_y")?;
            let and = builder.build_and(x, y, "x_and_y")?;
            let lhs = builder.build_or(xor, and, "xor_or_and")?;
            let rhs = builder.build_or(x, y, "x_or_y")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, lhs, rhs, "id_bitdecomp")?
        }
        // Identity 4: ((x ^ y) & x) | (x & y) == x  — always true
        3 => {
            let xor = builder.build_xor(x, y, "x_xor_y")?;
            let xor_and_x = builder.build_and(xor, x, "xor_and_x")?;
            let x_and_y = builder.build_and(x, y, "x_and_y")?;
            let lhs = builder.build_or(xor_and_x, x_and_y, "partition")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, lhs, x, "id_partition")?
        }
        // Identity 5: (x | y) >= (x & y)  — always true (unsigned)
        4 => {
            let or = builder.build_or(x, y, "x_or_y")?;
            let and = builder.build_and(x, y, "x_and_y")?;
            let pred = if negate { IntPredicate::ULT } else { IntPredicate::UGE };
            builder.build_int_compare(pred, or, and, "id_or_ge_and")?
        }
        // Identity 6: x*x - 1 == (x-1)*(x+1)  — always true (mod 2^32)
        5 => {
            let one = i32_type.const_int(1, false);
            let x_sq = builder.build_int_mul(x, x, "x_sq")?;
            let lhs = builder.build_int_sub(x_sq, one, "x_sq_m1")?;
            let x_m1 = builder.build_int_sub(x, one, "x_m1")?;
            let x_p1 = builder.build_int_add(x, one, "x_p1")?;
            let rhs = builder.build_int_mul(x_m1, x_p1, "diff_sq")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, lhs, rhs, "id_diffsq")?
        }
        // Identity 7: 2*(x & y) + (x ^ y) == x + y  — always true
        6 => {
            let two = i32_type.const_int(2, false);
            let and = builder.build_and(x, y, "x_and_y")?;
            let two_and = builder.build_int_mul(two, and, "two_and")?;
            let xor = builder.build_xor(x, y, "x_xor_y")?;
            let lhs = builder.build_int_add(two_and, xor, "add_decomp_lhs")?;
            let rhs = builder.build_int_add(x, y, "x_plus_y")?;
            let pred = if negate { IntPredicate::NE } else { IntPredicate::EQ };
            builder.build_int_compare(pred, lhs, rhs, "id_adddecomp")?
        }
        _ => unreachable!(),
    };

    // If negate is true, the condition is always-false; otherwise always-true
    Ok((!negate, condition))
}

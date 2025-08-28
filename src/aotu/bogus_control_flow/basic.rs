use crate::aotu::bogus_control_flow::{BogusControlFlow, BogusControlFlowAlgo};
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt, PhiInst};
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, GlobalValue, InstructionOpcode, IntValue, PointerValue};
use log::{error, warn};
use rand::Rng;

#[derive(Default)]
pub struct BogusControlFlowBasic {
    x: u32,
    y: u32,
}

impl BogusControlFlowAlgo for BogusControlFlowBasic {
    fn initialize(&mut self, pass: &BogusControlFlow, module: &mut Module<'_>) -> anyhow::Result<()> {
        self.x = rand::random_range(0..2782812982);
        self.y = rand::random_range(0..459846238);

        Ok(())
    }

    fn apply_bogus_control_flow(&mut self, pass: &BogusControlFlow, module: &mut Module<'_>) -> anyhow::Result<()> {
        // Create global variables for opaque predicates
        let globals = create_opaque_predicate_globals(self, module);

        for function in module.get_functions() {
            if function.count_basic_blocks() == 0 {
                continue;
            }

            for _ in 0..pass.loop_count {
                if let Err(e) = handle_function(self, function, &globals, pass.probability) {
                    warn!("(bogus-control-flow) failed to obfuscate function: {}", e);
                }
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
    let x_global = module.add_global(i32_type, None, "");
    x_global.set_initializer(&i32_type.const_int(algo.x as u64, false));

    let y_global = module.add_global(i32_type, None, "");
    y_global.set_initializer(&i32_type.const_int(algo.y as u64, false));

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
    let basic_blocks: Vec<BasicBlock> = function.get_basic_blocks();

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
        let terminator = entry_block.get_terminator().unwrap();
        let context = function.get_type().get_context();
        let i32_type = context.i32_type();
        let builder = context.create_builder();
        builder.position_before(&terminator);
        let x = builder.build_alloca(i32_type, "")?;
        let y = builder.build_alloca(i32_type, "")?;
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
    let i32_type = context.i32_type();
    let builder = context.create_builder();

    // Create new blocks
    let condition_block = context.append_basic_block(function, "bogus_cond");
    let fake_block = context.append_basic_block(function, "bogus_fake");

    // Build from original block to condition block
    // 这里因为我在后面删除了终结指令
    // 所以说不会出现因为在终结指令后面新增代码出现校验不通过的问题
    builder.position_at_end(original_block);
    builder.build_unconditional_branch(condition_block)?;
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
    let then_block = if is_true { target_bb } else { fake_block };
    let else_block = if is_true { fake_block } else { target_bb };
    builder.build_conditional_branch(condition, then_block, else_block)?;

    // Build fake block (should never be executed)
    builder.position_at_end(fake_block);
    // Add some junk instructions
    match rand::random_range(0..=10) {
        0..3 => {
            let junk1 = builder.build_load2(i32_type, globals.x.as_pointer_value(), "junk1")?;
            let junk2 = builder.build_load2(i32_type, globals.y.as_pointer_value(), "junk2")?;
            builder.build_store(stack.0, junk1)?;
            builder.build_store(stack.1, junk2)?;
            builder.build_unconditional_branch(fake_block)?;
        },
        3..7 => {
            let junk1 = builder.build_load2(i32_type, stack.0, "junk1")?;
            let junk2 = builder.build_load2(i32_type, stack.1, "junk2")?;
            builder.build_store(globals.y.as_pointer_value(), junk1)?;
            builder.build_store(globals.x.as_pointer_value(), junk2)?;
            builder.build_unconditional_branch(then_block)?;
        },
        7 => {
            let ret_type = function.get_type().get_return_type();
            if ret_type.is_none() {
                builder.build_return(None)?;
            } else {
                builder.build_unconditional_branch(else_block)?;
            }
        },
        8 => {
            builder.build_unreachable()?;
        },
        _ => {
            let val1 = i32_type.const_int(rand::random::<u32>() as u64, false);
            let val2 = i32_type.const_int(13, false);
            let junk1 = builder.build_int_add(val1, val2, "junk1")?;
            let junk2 = builder.build_int_mul(val1, val2, "junk2")?;
            builder.build_store(globals.x.as_pointer_value(), junk1)?;
            builder.build_store(globals.y.as_pointer_value(), junk2)?;
            builder.build_unconditional_branch(target_bb)?;
        },
    }

    target_bb.fix_phi_node(condition_block, fake_block);

    terminator.erase_from_basic_block();

    Ok(())
}

/// Create a simple opaque predicate that always evaluates to true
fn create_simple_opaque_predicate<'a>(
    algo: &BogusControlFlowBasic,
    builder: &Builder<'a>,
    x: PointerValue<'a>,
    y: PointerValue<'a>,
) -> anyhow::Result<(bool, IntValue<'a>)> {
    let context = builder.get_insert_block().unwrap().get_context();
    let i32_type = context.i32_type();

    let mut bits = [false; 2];
    rand::fill(&mut bits);

    // Load global variables
    let x_val = builder.build_load2(i32_type, x, "x_val")?.into_int_value();
    let y_val = builder.build_load2(i32_type, y, "y_val")?.into_int_value();

    let (opaque_val, val) = if bits[0] {
        (x_val, algo.x) // Use x if first bit is true
    } else {
        (y_val, algo.y) // Use y if first bit is false
    };

    let rand_val_u32 = rand::random_range(0..473948483);
    let is_gt = val > rand_val_u32;
    let is_lt = val < rand_val_u32;
    let is_eq = val == rand_val_u32;
    let rand_val = i32_type.const_int(rand_val_u32 as u64, false);

    // if log_enabled!(Level::Debug) {
    //     debug!("[bogus-control-flow] is_gt: {}, is_lt: {}, is_eq: {}", is_gt, is_lt, is_eq);
    // }
    // bits[1] is true ==> condition always true
    let (pred, lhs, rhs) = match (is_gt, is_lt, is_eq) {
        (true, false, false) => (
            IntPredicate::UGT,
            if bits[1] { opaque_val } else { rand_val },
            if bits[1] { rand_val } else { opaque_val },
        ),
        (false, true, false) => (
            IntPredicate::ULT,
            if bits[1] { opaque_val } else { rand_val },
            if bits[1] { rand_val } else { opaque_val },
        ),
        (false, false, true) => (
            if bits[1] { IntPredicate::EQ } else { IntPredicate::NE },
            if bits[1] { opaque_val } else { rand_val },
            if bits[1] { rand_val } else { opaque_val },
        ),
        _ => panic!("Invalid condition"),
    };

    let label = if bits[1] { "always_true" } else { "always_false" };
    let result = builder.build_int_compare(pred, lhs, rhs, label)?;

    // if log_enabled!(Level::Debug) {
    //     debug!("[bogus-control-flow] Condition: {} {} ({} {})", bits[1], label, lhs, rhs);
    // }

    Ok((bits[1], result))
}

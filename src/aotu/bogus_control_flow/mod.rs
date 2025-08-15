use crate::config::Config;
use crate::llvm_utils::branch_inst;
use crate::llvm_utils::function::get_basic_block_entry;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::{VerifyResult, verify_function};
use amice_macro::amice;
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::llvm_sys::core::LLVMAddIncoming;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::BasicType;
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicValue, FunctionValue, GlobalValue, InstructionOpcode, IntValue, PhiValue, PointerValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, log_enabled, warn, Level};
use rand::Rng;

#[amice(priority = 950, name = "BogusControlFlow", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct BogusControlFlow {
    enable: bool,
    probability: u32,
    loop_count: u32,
    x: u32,
    y: u32,
}

impl AmicePassLoadable for BogusControlFlow {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.bogus_control_flow.enable;
        self.probability = cfg.bogus_control_flow.probability;
        self.loop_count = cfg.bogus_control_flow.loop_count;

        if self.enable {
            debug!(
                "BogusControlFlow pass enabled with probability: {}%, loops: {}",
                self.probability, self.loop_count
            );
        }

        self.x = rand::random_range(0..2782812982);
        self.y = rand::random_range(0..459846238);

        self.enable
    }
}

impl LlvmModulePass for BogusControlFlow {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        // Create global variables for opaque predicates
        let globals = create_opaque_predicate_globals(self, module);

        for function in module.get_functions() {
            if function.count_basic_blocks() == 0 {
                continue;
            }

            for _ in 0..self.loop_count {
                if let Err(e) = handle_function(self, function, &globals, self.probability) {
                    warn!("(bogus-control-flow) failed to obfuscate function: {}", e);
                }
            }

            // Verify function after transformation
            if let VerifyResult::Broken(msg) = verify_function(function.as_value_ref() as *mut std::ffi::c_void) {
                warn!(
                    "(bogus-control-flow) function {:?} verify failed: {}",
                    function.get_name(),
                    msg
                );
            }
        }

        PreservedAnalyses::None
    }
}

/// Create global variables used for opaque predicates
fn create_opaque_predicate_globals<'a>(pass: &BogusControlFlow, module: &mut Module<'a>) -> OpaquePredicateGlobals<'a> {
    let context = module.get_context();
    let i32_type = context.i32_type();

    // Create global x and y variables for opaque predicates
    let x_global = module.add_global(i32_type, None, "");
    x_global.set_initializer(&i32_type.const_int(pass.x as u64, false));

    let y_global = module.add_global(i32_type, None, "");
    y_global.set_initializer(&i32_type.const_int(pass.y as u64, false));

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
    pass: &BogusControlFlow,
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
    let entry_block = get_basic_block_entry(function);

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
        builder.build_store(x, i32_type.const_int(pass.x as u64, false))?;
        builder.build_store(y, i32_type.const_int(pass.y as u64, false))?;
        (x, y)
    };

    // Apply bogus control flow to selected blocks
    for bb in blocks_to_modify {
        if let Err(e) = apply_bogus_control_flow_to_unconditional_branch(
            pass,
            function,
            bb,
            globals,
            &stack_predicates
        ) {
            warn!("Failed to apply bogus control flow to block: {}", e);
        }
    }

    Ok(())
}

/// Apply a simplified version of bogus control flow
fn apply_bogus_control_flow_to_unconditional_branch(
    pass: &BogusControlFlow,
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

    let target_bb = branch_inst::get_successor(terminator, 0)
        .and_then(|op| op.right())
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
    update_phi_nodes(original_block, condition_block, target_bb);

    // Build condition block with opaque predicate
    builder.position_at_end(condition_block);
    let (is_true, condition) = if rand::random::<bool>() {
        create_simple_opaque_predicate(
            pass,
            &builder,
            globals.x.as_pointer_value(),
            globals.y.as_pointer_value(),
        )?
    } else {
        create_simple_opaque_predicate(
            pass,
            &builder,
            stack.0,
            stack.1,
        )?
    };
    let then_block = if is_true { target_bb } else { fake_block };
    let else_block = if is_true { fake_block } else { target_bb };
    builder.build_conditional_branch(condition, then_block, else_block)?;

    // Build fake block (should never be executed)
    builder.position_at_end(fake_block);
    // Add some junk instructions
    match rand::random_range(0 ..= 10) {
        0..3 => {
            let junk1 = builder.build_load(i32_type, globals.x.as_pointer_value(), "junk1")?;
            let junk2 = builder.build_load(i32_type, globals.y.as_pointer_value(), "junk2")?;
            builder.build_store(stack.0, junk1)?;
            builder.build_store(stack.1, junk2)?;
            builder.build_unconditional_branch(fake_block)?;
        },
        3..7 => {
            let junk1 = builder.build_load(i32_type, stack.0, "junk1")?;
            let junk2 = builder.build_load(i32_type, stack.1, "junk2")?;
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
        }
        8 => {
            builder.build_unreachable()?;
        }
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

    update_phi_nodes(condition_block, fake_block, target_bb);

    terminator.erase_from_basic_block();

    Ok(())
}

fn update_phi_nodes<'ctx>(old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>, target_block: BasicBlock<'ctx>) {
    for phi in target_block.get_first_instruction().iter() {
        if phi.get_opcode() != InstructionOpcode::Phi {
            break;
        }

        let phi = unsafe { PhiValue::new(phi.as_value_ref()) };
        let incoming_vec = phi
            .get_incomings()
            .filter_map(|(value, pred)| {
                if pred == old_pred {
                    (value, new_pred).into()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let (mut values, mut basic_blocks): (Vec<LLVMValueRef>, Vec<LLVMBasicBlockRef>) = {
            incoming_vec
                .iter()
                .map(|&(v, bb)| (v.as_value_ref(), bb.as_mut_ptr()))
                .unzip()
        };

        unsafe {
            LLVMAddIncoming(
                phi.as_value_ref(),
                values.as_mut_ptr(),
                basic_blocks.as_mut_ptr(),
                incoming_vec.len() as u32,
            );
        }
    }
}

/// Create a simple opaque predicate that always evaluates to true
fn create_simple_opaque_predicate<'a>(
    pass: &BogusControlFlow,
    builder: &Builder<'a>,
    x: PointerValue<'a>,
    y: PointerValue<'a>,
) -> anyhow::Result<(bool, IntValue<'a>)> {
    let context = builder.get_insert_block().unwrap().get_context();
    let i32_type = context.i32_type();
    let i32_zero = i32_type.const_int(0, false);

    let mut bits = [false; 2];
    rand::fill(&mut bits);

    // Load global variables
    let x_val = builder.build_load(i32_type, x, "x_val")?.into_int_value();
    let y_val = builder.build_load(i32_type, y, "y_val")?.into_int_value();

    let (opaque_val, val) = if bits[0] {
        (x_val, pass.x) // Use x if first bit is true
    } else {
        (y_val, pass.y) // Use y if first bit is false
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

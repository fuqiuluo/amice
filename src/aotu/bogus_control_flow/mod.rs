use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::verify_function;
use amice_macro::amice;
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::BasicType;
use llvm_plugin::inkwell::values::{AsValueRef, BasicValue, FunctionValue, GlobalValue, IntValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, warn};
use rand::Rng;

#[amice(priority = 900, name = "BogusControlFlow", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct BogusControlFlow {
    enable: bool,
    probability: u32,
    loop_count: u32,
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

        self.enable
    }
}

impl LlvmModulePass for BogusControlFlow {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        // Create global variables for opaque predicates
        let globals = create_opaque_predicate_globals(module);

        for function in module.get_functions() {
            if function.count_basic_blocks() == 0 {
                continue;
            }

            for _ in 0..self.loop_count {
                if let Err(e) = handle_function(&function, &globals, self.probability) {
                    warn!("(bogus-control-flow) failed to obfuscate function: {}", e);
                }
            }

            // Verify function after transformation
            if verify_function(function.as_value_ref() as *mut std::ffi::c_void) {
                warn!("(bogus-control-flow) function {:?} verify failed", function.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

/// Create global variables used for opaque predicates
fn create_opaque_predicate_globals<'a>(module: &mut Module<'a>) -> OpaquePredicateGlobals<'a> {
    let context = module.get_context();
    let i32_type = context.i32_type();

    // Create global x and y variables for opaque predicates
    let x_global = module.add_global(i32_type, None, "amice_x");
    x_global.set_initializer(&i32_type.const_int(1, false));

    let y_global = module.add_global(i32_type, None, "amice_y");
    y_global.set_initializer(&i32_type.const_int(0, false));

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
    function: &FunctionValue<'_>,
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

    // Collect blocks to modify (excluding entry block)
    for bb in basic_blocks.iter().skip(1) {
        let random_value: u32 = rng.random();
        if (random_value % 100) < probability {
            blocks_to_modify.push(*bb);
        }
    }

    // Apply bogus control flow to selected blocks
    for bb in blocks_to_modify {
        if let Err(e) = apply_bogus_control_flow_simple(function, bb, globals) {
            warn!("Failed to apply bogus control flow to block: {}", e);
        }
    }

    Ok(())
}

/// Apply a simplified version of bogus control flow
fn apply_bogus_control_flow_simple(
    function: &FunctionValue<'_>,
    original_block: BasicBlock<'_>,
    globals: &OpaquePredicateGlobals<'_>,
) -> anyhow::Result<()> {
    let context = function.get_type().get_context();
    let builder = context.create_builder();

    // Get terminator instruction before we modify anything
    let terminator = original_block
        .get_terminator()
        .ok_or_else(|| anyhow::anyhow!("Block has no terminator"))?;

    // Only handle simple unconditional branches for now
    if terminator.get_num_operands() != 1 {
        return Ok(()); // Skip complex control flow
    }

    let target_bb = terminator
        .get_operand(0)
        .and_then(|op| op.right())
        .ok_or_else(|| anyhow::anyhow!("Cannot get branch target"))?;

    // Create new blocks
    let condition_block = context.append_basic_block(*function, "bogus_cond");
    let fake_block = context.append_basic_block(*function, "bogus_fake");

    // Remove the original terminator
    terminator.remove_from_basic_block();

    // Build from original block to condition block
    builder.position_at_end(original_block);
    builder.build_unconditional_branch(condition_block)?;

    // Build condition block with opaque predicate
    builder.position_at_end(condition_block);
    let condition = create_simple_opaque_predicate(&builder, globals)?;
    builder.build_conditional_branch(condition, target_bb, fake_block)?;

    // Build fake block (should never be executed)
    builder.position_at_end(fake_block);

    // Add some junk instructions
    let i32_type = context.i32_type();
    let val1 = i32_type.const_int(42, false);
    let val2 = i32_type.const_int(13, false);
    let _junk1 = builder.build_int_add(val1, val2, "junk1")?;
    let _junk2 = builder.build_int_mul(val1, val2, "junk2")?;

    // Jump to target (this should never execute)
    builder.build_unconditional_branch(target_bb)?;

    Ok(())
}

/// Create a simple opaque predicate that always evaluates to true
fn create_simple_opaque_predicate<'a>(
    builder: &Builder<'a>,
    globals: &OpaquePredicateGlobals<'a>,
) -> anyhow::Result<IntValue<'a>> {
    let context = builder.get_insert_block().unwrap().get_context();
    let i32_type = context.i32_type();

    // Load global variables
    let x_val = builder
        .build_load(i32_type, globals.x.as_pointer_value(), "x_val")?
        .into_int_value();
    let y_val = builder
        .build_load(i32_type, globals.y.as_pointer_value(), "y_val")?
        .into_int_value();

    // Simple predicate: y < 10 (should always be true since y = 0)
    let ten = i32_type.const_int(10, false);
    let result = builder.build_int_compare(IntPredicate::SLT, y_val, ten, "always_true")?;

    Ok(result)
}

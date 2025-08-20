use crate::config::{Config, ShuffleBlocksFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::{verify_function, verify_function2};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use amice_llvm::ir::function::get_basic_block_entry;

#[amice(priority = 970, name = "ShuffleBlocks", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct ShuffleBlocks {
    enable: bool,
    flags: ShuffleBlocksFlags,
}

impl AmicePassLoadable for ShuffleBlocks {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.shuffle_blocks.enable;
        self.flags = cfg.shuffle_blocks.flags;
        if self.flags.is_empty() {
            warn!("(shuffle-blocks) no flags set, disabling shuffle blocks");
            self.enable = false;
        }
        self.enable
    }
}

impl LlvmModulePass for ShuffleBlocks {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if function.count_basic_blocks() <= 1 {
                continue;
            }
            if let Err(e) = handle_function(function, self.flags) {
                error!("(shuffle-blocks) failed to shuffle basic blocks: {}", e);
            }
        }

        PreservedAnalyses::None
    }
}

fn handle_function(function: FunctionValue<'_>, flags: ShuffleBlocksFlags) -> anyhow::Result<()> {
    let mut blocks = function.get_basic_blocks();
    if blocks.is_empty() || blocks.len() <= 3 {
        return Ok(());
    }

    let entry_block = get_basic_block_entry(function).ok_or_else(|| anyhow::anyhow!("failed to get entry block"))?;
    blocks.retain(|block| block != &entry_block);

    if log_enabled!(Level::Debug) {
        debug!(
            "(shuffle-blocks) function {:?} has {:?} basic blocks: {:?}",
            function.get_name(),
            blocks.len(),
            flags
        );
    }

    if flags.contains(ShuffleBlocksFlags::Random) {
        // Shuffle blocks by Ylarod
        for i in (1..blocks.len()).rev() {
            let j = rand::random_range(0..=i);
            if i != j {
                // Move block at index i after block at index j
                // Get the next block after blocks[j] to use as insertion point
                let insert_after = if j < blocks.len() - 1 { Some(blocks[j]) } else { None };

                if let Some(after_block) = insert_after {
                    let _ = blocks[i].move_after(after_block);
                } else {
                    // Move to end if j is the last block
                    let _ = blocks[i].move_after(blocks[j]);
                }
            }
        }
    }

    if flags.contains(ShuffleBlocksFlags::Reverse) {
        // Reverse the order of blocks (excluding entry block which is already removed from blocks)
        // Move blocks from back to front to reverse the order
        for i in (1..blocks.len()).rev() {
            let first_block = blocks[0];
            let _ = blocks[i].move_before(first_block);
        }
    }

    if flags.contains(ShuffleBlocksFlags::Rotate) {
        // Rotate blocks left by 1 (move first block to end)
        if !blocks.is_empty() {
            let first_block = blocks[0];
            let last_block = blocks[blocks.len() - 1];
            let _ = first_block.move_after(last_block);
        }
    }

    if verify_function2(function.as_value_ref() as *mut std::ffi::c_void) {
        warn!("(shuffle-blocks) function {:?} is not verified", function.get_name());
    }

    Ok(())
}

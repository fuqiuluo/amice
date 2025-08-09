use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, log_enabled, warn, Level};
use crate::config::{ShuffleBlocksFlags, CONFIG};
use crate::llvm_utils::function::get_basic_block_entry;
use rand::seq::SliceRandom;

pub struct ShuffleBlocks {
    enable: bool,
    flags: ShuffleBlocksFlags,
}

impl LlvmModulePass for ShuffleBlocks {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All
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

impl ShuffleBlocks {
    pub fn new(mut enable: bool) -> Self {
        let flags = CONFIG.shuffle_blocks.flags;
        if flags.is_empty() {
            warn!("(shuffle-blocks) no flags set, disabling shuffle blocks");
            enable = false;
        }
        Self { enable, flags }
    }
}

fn handle_function(function: FunctionValue<'_>, flags: ShuffleBlocksFlags) -> anyhow::Result<()> {
    let mut blocks = function.get_basic_blocks();
    if blocks.is_empty() || blocks.len() <= 3 { return Ok(()); }

    let entry_block = get_basic_block_entry(function)
        .ok_or_else(|| anyhow::anyhow!("failed to get entry block"))?;
    blocks.retain(|block| block != &entry_block);

    if log_enabled!(Level::Debug) {
        debug!("(shuffle-blocks) function {:?} has {:?} basic blocks: {:?}", function.get_name(), blocks.len(), flags);
    }

    if flags.contains(ShuffleBlocksFlags::Random) {
        // Shuffle blocks by Ylarod
        for i in (1..blocks.len()).rev() {
            let j = rand::random_range(0..=i);
            if i != j {
                // Move block at index i after block at index j
                // Get the next block after blocks[j] to use as insertion point
                let insert_after = if j < blocks.len() - 1 {
                    Some(blocks[j])
                } else {
                    None
                };

                if let Some(after_block) = insert_after {
                    let _ = blocks[i].move_after(after_block);
                } else {
                    // Move to end if j is the last block
                    let _ = blocks[i].move_after(blocks[j]);
                }
            }
        }
    }

    Ok(())
}

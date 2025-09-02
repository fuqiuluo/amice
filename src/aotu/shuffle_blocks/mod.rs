use crate::config::{Config, ShuffleBlocksConfig, ShuffleBlocksFlags};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::FunctionExt;
use amice_macro::amice;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use log::{Level, log_enabled};

#[amice(
    priority = 970,
    name = "ShuffleBlocks",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = ShuffleBlocksConfig,
)]
#[derive(Default)]
pub struct ShuffleBlocks {}

impl AmicePass for ShuffleBlocks {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.shuffle_blocks.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut executed = false;
        for function in module.get_functions() {
            if function.is_inline_marked() || function.is_llvm_function() || function.is_undef_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            if function.count_basic_blocks() <= 1 {
                continue;
            }

            if let Err(e) = handle_function(function, cfg.flags) {
                error!("failed to shuffle basic blocks: {}", e);
            }
            executed = true;
        }
        
        if !executed {
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

fn handle_function(function: FunctionValue<'_>, flags: ShuffleBlocksFlags) -> anyhow::Result<()> {
    let mut blocks = function.get_basic_blocks();
    if blocks.is_empty() || blocks.len() <= 3 {
        return Ok(());
    }

    let entry_block = function
        .get_entry_block()
        .ok_or_else(|| anyhow::anyhow!("failed to get entry block"))?;
    blocks.retain(|block| block != &entry_block);

    if log_enabled!(Level::Debug) {
        debug!(
            "function {:?} has {:?} basic blocks: {:?}",
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

    if function.verify_function_bool() {
        warn!("function {:?} is not verified", function.get_name());
    }

    Ok(())
}

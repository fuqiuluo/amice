use crate::config::CONFIG;
use crate::llvm_utils::basic_block::split_basic_block;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled};
use rand::seq::SliceRandom;

pub struct SplitBasicBlock {
    enable: bool,
    split_num: u32,
}

impl LlvmModulePass for SplitBasicBlock {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(e) = do_split(module, function, self.split_num) {
                error!(
                    "Failed to split basic blocks in function {}: {}",
                    function.get_name().to_str().unwrap_or("unknown"),
                    e
                );
            }
        }

        PreservedAnalyses::None
    }
}

fn do_split(module: &mut Module<'_>, function: FunctionValue, split_num: u32) -> anyhow::Result<()> {
    for bb in function.get_basic_blocks() {
        let mut count = 0u32;
        if flitter_basic_block(&bb, split_num, &mut count) {
            continue;
        }

        let mut split_points: Vec<u32> = (1..count).collect();
        shuffle(&mut split_points);
        split_points.truncate(split_num as usize);
        split_points.sort_unstable();

        let mut to_split = bb;
        let mut it = bb.get_first_instruction();
        let mut last = 0u32;
        for i in 0..split_num {
            if count_instructions(&to_split) < 2 {
                break;
            }
            for j in 0..(split_points[i as usize] - last) {
                if let Some(curr_inst) = it {
                    it = curr_inst.get_next_instruction();
                } else {
                    break; // No more instructions to process
                }
            }
            last = split_points[i as usize];
            if let Some(curr_inst) = it {
                if let Some(new_block) = split_basic_block(to_split, curr_inst, ".split", false) {
                    to_split = new_block;
                } else {
                    error!("Failed to split basic block at point {last}");
                    break;
                }
            } else {
                error!("No instruction found to split at point {last}");
                break; // No more instructions to split
            }
        }
        if log_enabled!(Level::Debug) {
            debug!("{:?} split points: {:?}", bb.get_name(), split_points);
        }
    }
    Ok(())
}

pub fn shuffle(vec: &mut [u32]) {
    let mut rng = rand::rng();
    vec.shuffle(&mut rng);
}

fn flitter_basic_block(bb: &BasicBlock<'_>, split_num: u32, x: &mut u32) -> bool {
    bb.get_instructions().any(|inst| {
        *x += 1;
        inst.get_opcode() == InstructionOpcode::Phi
    }) || *x < 2
        || split_num > *x
}

fn count_instructions(bb: &BasicBlock<'_>) -> u32 {
    bb.get_instructions().count() as u32
}

impl SplitBasicBlock {
    pub fn new(enable: bool) -> Self {
        let split_num = CONFIG.split_basic_block.num;
        Self { enable, split_num }
    }
}

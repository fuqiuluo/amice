use crate::config::CONFIG;
use crate::llvm_utils::basic_block::split_basic_block;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled};
use rand::seq::SliceRandom;
use amice_llvm::ir::function::{fix_stack, fix_stack_at_terminator, fix_stack_with_max_iterations};

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

        // 确保基本块有合适的终结符
        if !has_valid_terminator(&bb) {
            continue;
        }

        let mut split_points: Vec<u32> = (1..count.saturating_sub(1)).collect(); // 避免切割最后一条指令
        shuffle(&mut split_points);
        split_points.truncate(split_num as usize);
        split_points.sort_unstable();

        let mut to_split = bb;
        let mut it = bb.get_first_instruction();
        let mut last = 0u32;

        for (i, &split_point) in split_points.iter().enumerate() {
            if count_instructions(&to_split) < 3 { // 确保至少有3条指令
                break;
            }

            // 移动到切割点
            for _ in 0..(split_point - last) {
                if let Some(curr_inst) = it {
                    it = curr_inst.get_next_instruction();
                } else {
                    break;
                }
            }

            last = split_point;

            if let Some(curr_inst) = it {
                // 确保不在终结符处切割
                if is_terminator_instruction(&curr_inst) {
                    continue;
                }

                let split_name = format!(".split_{}", i);
                if let Some(new_block) = split_basic_block(to_split, curr_inst, &split_name, false) {
                    to_split = new_block;
                } else {
                    error!("Failed to split basic block at point {}", split_point);
                    break;
                }
            } else {
                break;
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

fn is_terminator_instruction(inst: &llvm_plugin::inkwell::values::InstructionValue) -> bool {
    matches!(inst.get_opcode(),
        InstructionOpcode::Return |
        InstructionOpcode::Br |
        InstructionOpcode::Switch |
        InstructionOpcode::IndirectBr |
        InstructionOpcode::Invoke |
        InstructionOpcode::Resume |
        InstructionOpcode::Unreachable
    )
}

fn flitter_basic_block(bb: &BasicBlock<'_>, split_num: u32, x: &mut u32) -> bool {
    let has_problematic_instructions = bb.get_instructions().any(|inst| {
        *x += 1;
        match inst.get_opcode() {
            InstructionOpcode::Phi => true,
            InstructionOpcode::IndirectBr => true,  // 避免切割间接跳转
            InstructionOpcode::Switch => true,      // 避免切割switch
            InstructionOpcode::Invoke => true,      // 避免切割invoke
            _ => false,
        }
    });

    has_problematic_instructions || *x < 2 || split_num > *x
}

fn has_valid_terminator(bb: &BasicBlock<'_>) -> bool {
    bb.get_terminator().is_some()
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

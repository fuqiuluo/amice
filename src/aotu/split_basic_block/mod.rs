use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use rand::seq::SliceRandom;

#[amice(priority = 980, name = "SplitBasicBlock", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct SplitBasicBlock {
    enable: bool,
    split_num: u32,
}

impl AmicePassLoadable for SplitBasicBlock {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.split_basic_block.enable;
        self.split_num = cfg.split_basic_block.num;

        self.enable
    }
}

impl LlvmModulePass for SplitBasicBlock {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(e) = do_split(module, function, self.split_num) {
                error!(
                    "Failed to split basic blocks in function {:?}: {}",
                    function.get_name(),
                    e
                );
            }
        }

        for f in module.get_functions() {
            if f.verify_function_bool() {
                warn!("(split-basic-block) function {:?} is not verified", f.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

fn do_split(module: &mut Module<'_>, function: FunctionValue, split_num: u32) -> anyhow::Result<()> {
    let Some(entry) = function.get_entry_block() else {
        return Err(anyhow!("Function {:?} has no entry block", function.get_name()));
    };

    for bb in function.get_basic_blocks() {
        let is_entry = entry == bb;

        // 非入口块上如发现 alloca，跳过
        if !is_entry && block_has_alloca(&bb) {
            continue;
        }

        let mut count = 0u32;
        if flitter_basic_block(&bb, split_num, &mut count) {
            // 注意：对于入口块，flitter_basic_block 检出 Alloca 会返回 true
            // 但我们后面仍可能允许在 alloca 之后切（见下）
            if !(is_entry && count > 0) {
                continue;
            }
        }

        // 确保基本块有合适的终结符
        if !has_valid_terminator(&bb) {
            continue;
        }

        // 构造候选切点
        let mut split_points: Vec<u32> = (1..count.saturating_sub(1)).collect(); // 避免切割最后一条指令

        // 入口块：仅允许在首个非 alloca 指令之后切割
        let start_idx = if is_entry { first_non_alloca_index(&bb) } else { 0 };
        split_points.retain(|&i| i > start_idx);

        if split_points.is_empty() {
            continue;
        }

        shuffle(&mut split_points);
        split_points.truncate(split_num as usize);
        split_points.sort_unstable();

        let mut to_split = bb;
        let mut it = bb.get_first_instruction();
        let mut last = 0u32;

        for (i, &split_point) in split_points.iter().enumerate() {
            if count_instructions(&to_split) < 3 {
                // 确保至少有3条指令
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

                if curr_inst.get_opcode() == InstructionOpcode::Alloca {
                    // 永不在 alloca 处切割
                    continue;
                }

                let split_name = format!(".split_{}", i);
                if let Some(new_block) = to_split.split_basic_block(curr_inst, &split_name, false) {
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

fn block_has_alloca(bb: &BasicBlock<'_>) -> bool {
    bb.get_instructions()
        .any(|inst| inst.get_opcode() == InstructionOpcode::Alloca)
}

fn first_non_alloca_index(bb: &BasicBlock<'_>) -> u32 {
    let mut idx = 0u32;
    for inst in bb.get_instructions() {
        match inst.get_opcode() {
            InstructionOpcode::Alloca => idx += 1,
            _ => break,
        }
    }
    idx
}

fn is_terminator_instruction(inst: &llvm_plugin::inkwell::values::InstructionValue) -> bool {
    matches!(
        inst.get_opcode(),
        InstructionOpcode::Return
            | InstructionOpcode::Br
            | InstructionOpcode::Switch
            | InstructionOpcode::IndirectBr
            | InstructionOpcode::Invoke
            | InstructionOpcode::Resume
            | InstructionOpcode::Unreachable
    )
}

fn flitter_basic_block(bb: &BasicBlock<'_>, split_num: u32, x: &mut u32) -> bool {
    let has_problematic_instructions = bb.get_instructions().any(|inst| {
        *x += 1;
        match inst.get_opcode() {
            InstructionOpcode::Phi => true,
            InstructionOpcode::IndirectBr => true, // 避免切割间接跳转
            InstructionOpcode::Switch => true,     // 避免切割switch
            InstructionOpcode::Invoke => true,     // 避免切割invoke
            InstructionOpcode::Alloca => {
                // 非入口块上有 alloca，一律视为问题块
                // 调用处可根据 bb 是否为入口块决定是否跳过
                true
            },
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

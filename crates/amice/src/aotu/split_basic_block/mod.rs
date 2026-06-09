use crate::config::{Config, SplitBasicBlockConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt};
use amice_macro::amice;
use amice_plugin::PreservedAnalyses;
use amice_plugin::inkwell::basic_block::BasicBlock;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use anyhow::anyhow;
use log::{Level, log_enabled};
use rand::seq::SliceRandom;

#[amice(
    priority = 980,
    name = "SplitBasicBlock",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::OptimizerLast | AmicePassFlag::FunctionLevel,
    config = SplitBasicBlockConfig,
)]
#[derive(Default)]
pub struct SplitBasicBlock {}

impl AmicePass for SplitBasicBlock {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.split_basic_block.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut has_executed = false;
        for function in module.get_functions() {
            if function.is_undef_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            if let Err(e) = do_split(module, function, cfg.num) {
                error!(
                    "Failed to split basic blocks in function {:?}: {}",
                    function.get_name(),
                    e
                );
            }

            has_executed = true;
            if function.verify_function_bool() {
                warn!("function {:?} is not verified", function.get_name());
            }
        }

        if !has_executed {
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

fn do_split(_module: &mut Module<'_>, function: FunctionValue, split_num: u32) -> anyhow::Result<()> {
    let Some(entry) = function.get_entry_block() else {
        return Err(anyhow!("Function {:?} has no entry block", function.get_name()));
    };

    for bb in function.get_basic_blocks() {
        let is_entry = entry == bb;

        let mut count = 0u32;
        if flitter_basic_block(&bb, split_num, is_entry, &mut count) {
            continue;
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

fn shuffle(vec: &mut [u32]) {
    let mut rng = rand::rng();
    vec.shuffle(&mut rng);
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

fn is_terminator_instruction(inst: &amice_plugin::inkwell::values::InstructionValue) -> bool {
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

fn flitter_basic_block(bb: &BasicBlock<'_>, split_num: u32, is_entry: bool, x: &mut u32) -> bool {
    let mut has_problematic_instructions = false;
    let mut seen_non_alloca = false;

    for inst in bb.get_instructions() {
        *x += 1;
        match inst.get_opcode() {
            InstructionOpcode::Phi
            | InstructionOpcode::IndirectBr
            | InstructionOpcode::Switch
            | InstructionOpcode::Invoke => {
                has_problematic_instructions = true;
            },
            InstructionOpcode::Alloca if !is_entry || seen_non_alloca => {
                has_problematic_instructions = true;
            },
            InstructionOpcode::Alloca => {},
            _ => seen_non_alloca = true,
        }
    }

    has_problematic_instructions || *x < 2 || split_num > *x
}

fn has_valid_terminator(bb: &BasicBlock<'_>) -> bool {
    bb.get_terminator().is_some()
}

fn count_instructions(bb: &BasicBlock<'_>) -> u32 {
    bb.get_instructions().count() as u32
}

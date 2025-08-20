mod cf_flatten_basic;
mod cf_flatten_dominator;

use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::{Config, FlattenMode, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::function::fix_stack;
use amice_llvm::module_utils::{VerifyResult, verify_function, verify_function2};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, IntValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use rand::Rng;
use std::collections::HashMap;
use amice_llvm::ir::basic_block::split_basic_block;

#[amice(priority = 959, name = "Flatten", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct Flatten {
    enable: bool,
    fix_stack: bool,
    demote_switch: bool,
    mode: FlattenMode,
    loop_count: usize,
    skip_big_function: bool,
}

impl AmicePassLoadable for Flatten {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.flatten.enable;
        self.fix_stack = cfg.flatten.fix_stack;
        self.demote_switch = cfg.flatten.lower_switch;

        if !self.fix_stack && !self.demote_switch {
            // switch降级没有开启且fixStack也没有开启意味着PHI 99%有问题！
            error!(
                "(flatten) both fix_stack and lower_switch are disabled, this will likely cause issues with PHI nodes"
            );
            // 给个警告，然后听天由命，这个是用户自己决定的，hhh
        }

        self.mode = cfg.flatten.mode;
        self.loop_count = cfg.flatten.loop_count;
        self.skip_big_function = cfg.flatten.skip_big_function;

        self.enable
    }
}

impl LlvmModulePass for Flatten {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        'out: for function in module.get_functions() {
            if function.count_basic_blocks() <= 2 {
                continue;
            }

            if self.skip_big_function && function.count_basic_blocks() > 4096 {
                continue
            }

            for _ in 0..self.loop_count {
                if let Err(err) = match self.mode {
                    FlattenMode::Basic => cf_flatten_basic::do_handle(module, function, self.demote_switch),
                    FlattenMode::DominatorEnhanced => cf_flatten_dominator::do_handle(module, function, self.demote_switch),
                } {
                    warn!("(flatten) function {:?} failed: {}", function.get_name(), err);
                    continue 'out;
                }

                if self.skip_big_function && function.count_basic_blocks() > 4096 {
                    break
                }
            }

            if self.fix_stack {
                unsafe {
                    fix_stack(function.as_value_ref() as *mut std::ffi::c_void);
                }
            }

            if let VerifyResult::Broken(e) = verify_function(function.as_value_ref() as *mut std::ffi::c_void) {
                warn!("(flatten) function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        PreservedAnalyses::None
    }
}


fn split_entry_block_for_flatten<'a>(
    function: FunctionValue<'a>,
    entry_block: BasicBlock<'a>,
    basic_blocks: &mut Vec<BasicBlock<'a>>,
) -> anyhow::Result<bool> {
    let Some(entry_terminator) = entry_block.get_terminator() else {
        // 没有终结指令，居然还能通过上一层的基本块数量大于2的校验？！
        // 估计是别的Pass干的好事！
        return Ok(false);
    };

    // 调试一下入口块的终结指令
    //debug!("terminator: {:?}", entry_terminator);

    // 计算入口块指令数（用于决定 split 位置）
    let entry_block_inst_count = entry_block.get_instructions().count();

    let mut first_basic_block = None;
    match entry_terminator.get_opcode() {
        InstructionOpcode::Br => {
            if entry_terminator.is_conditional() || entry_terminator.get_num_operands() > 1 {
                // 分裂，让新块只承载 terminator，便于作为起始节点
                let mut split_pos = entry_terminator;
                if entry_block_inst_count > 0 {
                    split_pos = split_pos.get_previous_instruction().unwrap();
                }
                let Some(new_block) = split_basic_block(entry_block, split_pos, ".no.conditional.br", false) else {
                    panic!("failed to split basic block");
                };
                if new_block.get_parent().unwrap() != function {
                    return Err(anyhow!("Split block has wrong parent"));
                }
                first_basic_block = new_block.into();
            } else {
                // 无条件跳转，直接取目标块为第一个实际执行的块
                first_basic_block = entry_terminator
                    .get_operand(0)
                    .ok_or(anyhow!("expected operand for unconditional br"))?
                    .right()
                    .ok_or(anyhow!("expected right operand for unconditional br"))?
                    .into();
            }
        },
        InstructionOpcode::Switch => {
            // 这些 terminator 没有 单一落地块 概念，为保持与 br的 一致的处理，
            // 分裂出仅包含 terminator 的新块作为 first_basic_block
            let mut split_pos = entry_terminator;
            if entry_block_inst_count > 0 {
                split_pos = split_pos.get_previous_instruction().unwrap();
            }
            let Some(new_block) = split_basic_block(entry_block, split_pos, ".no.conditional.term", false) else {
                panic!("failed to split basic block");
            };
            if new_block.get_parent().unwrap() != function {
                return Err(anyhow!("Split block has wrong parent"));
            }
            first_basic_block = new_block.into();
        },
        InstructionOpcode::Return | InstructionOpcode::Unreachable => {
            // 无后继，不需要做 flatten
            return Ok(false);
        },
        _ => return Ok(false),
    }

    let Some(first_basic_block) = first_basic_block.take() else {
        return Err(anyhow::anyhow!("failed to get first basic block: {entry_terminator}"));
    };
    if !basic_blocks.contains(&first_basic_block) {
        basic_blocks.insert(0, first_basic_block);
    }

    Ok(true)
}
mod cf_flatten_basic;
mod cf_flatten_dominator;

use crate::aotu::flatten::cf_flatten_basic::FlattenBasic;
use crate::aotu::flatten::cf_flatten_dominator::FlattenDominator;
use crate::config::{Config, FlattenMode};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, VerifyResult};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{error, warn};

#[amice(priority = 959, name = "Flatten", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct Flatten {
    enable: bool,
    fix_stack: bool,
    demote_switch: bool,
    mode: FlattenMode,
    loop_count: usize,
    skip_big_function: bool,
    inline_fn: bool,
}

impl AmicePassLoadable for Flatten {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
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
        self.inline_fn = cfg.flatten.always_inline;

        self.enable
    }
}

impl LlvmModulePass for Flatten {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mut algo: Box<dyn FlattenAlgo> = match self.mode {
            FlattenMode::Basic => Box::new(FlattenBasic::default()),
            FlattenMode::DominatorEnhanced => Box::new(FlattenDominator::default()),
        };
        if let Err(e) = algo.initialize(self, module) {
            error!("(flatten) initialize failed: {}", e);
            return PreservedAnalyses::None;
        }

        if let Err(e) = algo.do_flatten(self, module) {
            error!("(flatten) do_flatten failed: {}", e);
            return PreservedAnalyses::None;
        }

        for x in module.get_functions() {
            if let VerifyResult::Broken(e) = x.verify_function() {
                warn!("(flatten) function {:?} verify failed: {}", x.get_name(), e);
            }
        }

        PreservedAnalyses::None
    }
}

pub(super) trait FlattenAlgo {
    fn initialize(&mut self, pass: &Flatten, module: &mut Module<'_>) -> anyhow::Result<()>;

    fn do_flatten(&mut self, pass: &Flatten, module: &mut Module<'_>) -> anyhow::Result<()>;
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
                let Some(new_block) = entry_block.split_basic_block(split_pos, ".no.conditional.br", false) else {
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
            let Some(new_block) = entry_block.split_basic_block(split_pos, ".no.conditional.term", false) else {
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

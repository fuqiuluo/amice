mod cf_flatten_basic;
mod cf_flatten_dominator;

use crate::aotu::flatten::cf_flatten_basic::FlattenBasic;
use crate::aotu::flatten::cf_flatten_dominator::FlattenDominator;
use crate::config::{Config, FlattenConfig, FlattenMode};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, VerifyResult};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(
    priority = 959,
    name = "Flatten",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = FlattenConfig,
)]
#[derive(Default)]
pub struct Flatten {}

impl AmicePass for Flatten {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.flatten.clone();

        if !self.default_config.fix_stack && !self.default_config.lower_switch {
            // switch降级没有开启且fixStack也没有开启意味着PHI 99%有问题！
            error!("both fix_stack and lower_switch are disabled, this will likely cause issues with PHI nodes");
            // 给个警告，然后听天由命，这个是用户自己决定的，hhh
        }
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for x in module.get_functions() {
            if x.is_llvm_function() || x.is_undef_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, x)?;
            if !cfg.enable {
                continue;
            }

            functions.push((x, cfg))
        }

        if functions.is_empty() {
            return Ok(PreservedAnalyses::All);
        }

        for (function, cfg) in functions {
            let mut algo: Box<dyn FlattenAlgo> = match cfg.mode {
                FlattenMode::Basic => Box::new(FlattenBasic::default()),
                FlattenMode::DominatorEnhanced => Box::new(FlattenDominator::default()),
            };
            if let Err(e) = algo.initialize(&cfg, module) {
                error!("initialize failed: {}", e);
                continue;
            }

            if let Err(e) = algo.do_flatten(&cfg, module, function) {
                error!("do_flatten failed: {}", e);
                continue;
            }

            if let VerifyResult::Broken(e) = function.verify_function() {
                warn!("function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

pub(super) trait FlattenAlgo {
    fn initialize(&mut self, cfg: &FlattenConfig, module: &mut Module<'_>) -> anyhow::Result<()>;

    fn do_flatten(
        &mut self,
        cfg: &FlattenConfig,
        module: &mut Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()>;
}

// LLVM 会自动在 A 的末尾插入一条 无条件跳转指令 (Unconditional Branch)，目标指向 B
fn split_entry_block_for_flatten<'a>(
    function: FunctionValue<'a>,
    entry_block: BasicBlock<'a>,
    basic_blocks: &mut Vec<BasicBlock<'a>>,
) -> anyhow::Result<Option<BasicBlock<'a>>> {
    let Some(entry_terminator) = entry_block.get_terminator() else {
        // 没有终结指令，居然还能通过上一层的基本块数量大于2的校验？！
        // 估计是别的Pass干的好事！
        return Ok(None);
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
                    .block()
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
            return Ok(None);
        },
        _ => {
            debug!("Unknown terminator opcode: {:?}", entry_terminator.get_opcode());
            return Ok(None);
        },
    }

    // 确保这个块包含在待处理列表中
    if let Some(start_block) = first_basic_block {
        if !basic_blocks.contains(&start_block) {
            basic_blocks.push(start_block);
        }
        // 不需要在这里强制 insert(0)，因为我们已经通过返回值告诉调用者是谁了
    }

    Ok(first_basic_block)
}

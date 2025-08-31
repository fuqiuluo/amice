use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::code_extractor::CodeExtractor;
use amice_llvm::inkwell2::{FunctionExt, VerifyResult};
use amice_macro::amice;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, warn};
use std::ops::Not;

#[amice(priority = 979, name = "BasicBlockOutlining", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct BasicBlockOutlining {
    enable: bool,
    max_extractor_size: usize,
}

impl AmicePassLoadable for BasicBlockOutlining {
    fn init(&mut self, cfg: &crate::config::Config, _position: PassPosition) -> bool {
        self.enable = cfg.basic_block_outlining.enable;
        self.max_extractor_size = cfg.basic_block_outlining.max_extractor_size;
        self.enable
    }
}

impl LlvmModulePass for BasicBlockOutlining {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if self.enable.not() {
            return PreservedAnalyses::All;
        }

        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_undef_function() || function.is_llvm_function() && function.is_inline_marked() {
                continue;
            }

            let mut inst_count = 0;
            for bb in function.get_basic_blocks() {
                inst_count += bb.get_instructions().count();
            }

            if inst_count <= 8 {
                continue;
            }

            functions.push(function);
        }

        for function in functions {
            if let Err(e) = do_outline(module, function, self.max_extractor_size) {
                error!("(bb-outlining) outline func {:?} failed: {}", function.get_name(), e);
            }

            if let VerifyResult::Broken(e) = function.verify_function() {
                error!("(bb-outlining) function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        PreservedAnalyses::None
    }
}

fn do_outline<'a>(
    module: &mut Module<'_>,
    function: FunctionValue<'a>,
    max_extractor_size: usize,
) -> anyhow::Result<()> {
    let mut bbs = function
        .get_basic_blocks()
        .iter()
        .map(|bb| *bb)
        .map(|bb| (bb.get_instructions().count(), bb))
        .filter(|bb| bb.0 > 4)
        .filter(|bb| {
            let tmp_bbs = vec![bb.1];
            let Some(ce) = CodeExtractor::new(&tmp_bbs) else {
                return false;
            };
            ce.is_eligible()
        })
        .collect::<Vec<_>>();

    if bbs.is_empty() {
        return Ok(());
    }

    bbs.sort_unstable_by_key(|bb| std::cmp::Reverse(bb.0));
    if bbs.len() > max_extractor_size {
        // 保留最大的前 max_extractor_size 个
        bbs.truncate(max_extractor_size);
    }

    if log::log_enabled!(Level::Debug) {
        debug!(
            "{:?} bbs to outline: {:?}",
            function.get_name(),
            bbs.iter().map(|bb| bb.0).collect::<Vec<_>>()
        );
    }

    for (_, bb) in bbs {
        let tmp_bbs = vec![bb];
        let Some(ce) = CodeExtractor::new(&tmp_bbs) else {
            continue;
        };

        if !ce.is_eligible() {
            continue;
        }

        if let Some(new_function) = ce.extract_code_region(function) {
            let ctx = module.get_context();
            let noinline_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("noinline"), 0);
            new_function.add_attribute(AttributeLoc::Function, noinline_attr);
        } else {
            warn!(
                "(bb-outlining) failed to extract code region from function {:?}",
                function.get_name()
            );
        }
    }

    Ok(())
}

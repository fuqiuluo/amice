use crate::config::{Config, FlattenMode, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(priority = 1112, name = "AliasAccess", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct AliasAccess {
    enable: bool,
}

impl AmicePassLoadable for AliasAccess {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.alias_access.enable;

        self.enable
    }
}

impl LlvmModulePass for AliasAccess {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        PreservedAnalyses::None
    }
}

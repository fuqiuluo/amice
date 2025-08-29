// translate from AmaObfuscatePass

use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(priority = 1150, name = "DelayOffsetLoading", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct DelayOffsetLoading {
    enable: bool,
}

impl AmicePassLoadable for DelayOffsetLoading {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.delay_offset_loading.enable;
        self.enable
    }
}

impl LlvmModulePass for DelayOffsetLoading {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        PreservedAnalyses::None
    }
}

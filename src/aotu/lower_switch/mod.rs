use std::process::id;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::warn;
use amice_macro::amice;
use crate::config::{Config};
use crate::pass_registry::AmicePassLoadable;

#[amice(priority = 961, name = "LowerSwitch")]
#[derive(Default)]
pub struct LowerSwitch {
    enable: bool,
}

impl AmicePassLoadable for LowerSwitch {
    fn init(&mut self, cfg: &Config) -> bool {
        self.enable = cfg.lower_switch.enable;

        self.enable
    }
}

impl LlvmModulePass for LowerSwitch {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }
        
        
        PreservedAnalyses::None
    }
}
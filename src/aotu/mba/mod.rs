mod const_utils;

use crate::pass_registry::AmicePassLoadable;
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use rand::prelude::*;
use std::fmt;

#[amice(priority = 955, name = "Mba")]
#[derive(Default)]
pub struct Mba {
    enable: bool,
}

impl AmicePassLoadable for Mba {
    fn init(&mut self, cfg: &crate::config::Config) -> bool {
        self.enable = cfg.mba.enable;

        self.enable
    }
}

impl LlvmModulePass for Mba {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }



        PreservedAnalyses::None
    }
}

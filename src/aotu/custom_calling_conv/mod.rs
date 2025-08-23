use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use amice_macro::amice;
use crate::aotu::clone_function::CloneFunction;
use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};

#[amice(priority = 1121, name = "CustomCallingConv", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct CustomCallingConv {
    enable: bool,
}

impl AmicePassLoadable for CustomCallingConv {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.custom_calling_conv.enable;

        self.enable
    }
}

impl LlvmModulePass for CustomCallingConv {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::None;
        }


        PreservedAnalyses::None
    }
}
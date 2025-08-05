use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

pub struct SplitBasicBlock {
    enable: bool,
}

impl LlvmModulePass for SplitBasicBlock {
    fn run_pass(
        &self,
        module: &mut Module<'_>,
        manager: &ModuleAnalysisManager,
    ) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        PreservedAnalyses::None
    }
}

impl SplitBasicBlock {
    pub fn new(enable: bool) -> Self {
        Self { enable }
    }
}

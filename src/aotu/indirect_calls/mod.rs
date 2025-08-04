use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

pub struct IndirectCalls {
    enable: bool,
}

impl LlvmModulePass for IndirectCalls {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {

        PreservedAnalyses::All
    }
}
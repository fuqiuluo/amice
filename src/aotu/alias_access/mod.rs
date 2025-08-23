mod pointer_chain;

use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::{VerifyResult, verify_function};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{error, warn};

#[amice(priority = 1112, name = "AliasAccess", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct AliasAccess {
    enable: bool,
    /// 打乱RawBox顺序
    shuffle_raw_box: bool,
    /// 宽松的RawBox，填充垃圾
    loose_raw_box: bool,
}

impl AmicePassLoadable for AliasAccess {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.alias_access.enable;

        self.shuffle_raw_box = false; // todo: 暂时先关着
        self.loose_raw_box = false;

        self.enable
    }
}

impl LlvmModulePass for AliasAccess {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(e) = pointer_chain::do_alias_access(self, module, function) {
                error!("(alias-access) failed to process function {:?}: {}", function.get_name(), e);
                continue;
            }

            if let VerifyResult::Broken(e) = verify_function(function) {
                warn!("(alias-access) function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        PreservedAnalyses::None
    }
}

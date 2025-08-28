mod pointer_chain;

use crate::aotu::alias_access::pointer_chain::PointerChainAlgo;
use crate::config::{AliasAccessMode, Config};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::{VerifyResult};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{error, warn};
use amice_llvm::inkwell2::FunctionExt;

#[amice(priority = 1112, name = "AliasAccess", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct AliasAccess {
    enable: bool,
    mode: AliasAccessMode,
    shuffle_raw_box: bool,
    loose_raw_box: bool,
}

impl AmicePassLoadable for AliasAccess {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.alias_access.enable;

        self.shuffle_raw_box = cfg.alias_access.shuffle_raw_box;
        self.loose_raw_box = cfg.alias_access.loose_raw_box;

        self.enable
    }
}

impl LlvmModulePass for AliasAccess {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mut algo: Box<dyn AliasAccessAlgo> = match self.mode {
            AliasAccessMode::PointerChain => Box::new(PointerChainAlgo::default()),
        };

        if let Err(e) = algo.initialize(self) {
            error!("(alias-access) failed to initialize: {}", e);
            return PreservedAnalyses::All;
        }

        if let Err(e) = algo.do_alias_access(self, module) {
            error!("(alias-access) failed to do alias access: {}", e);
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let VerifyResult::Broken(e) = function.verify_function() {
                warn!("(alias-access) function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        PreservedAnalyses::None
    }
}

pub(crate) trait AliasAccessAlgo {
    fn initialize(&mut self, pass: &AliasAccess) -> anyhow::Result<()>;

    fn do_alias_access(&mut self, pass: &AliasAccess, module: &Module<'_>) -> anyhow::Result<()>;
}

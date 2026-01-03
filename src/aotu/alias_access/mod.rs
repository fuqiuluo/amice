mod pointer_chain;

use crate::aotu::alias_access::pointer_chain::PointerChainAlgo;
use crate::config::{AliasAccessConfig, AliasAccessMode, Config};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag, AmicePassMetadata};
use amice_llvm::inkwell2::{FunctionExt, VerifyResult};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(
    priority = 1112,
    name = "AliasAccess",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = AliasAccessConfig,
)]
#[derive(Default)]
pub struct AliasAccess {}

impl AmicePass for AliasAccess {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.alias_access.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_llvm_function() {
                continue;
            }

            if function.is_inline_marked() {
                continue;
            }

            if function.is_undef_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            functions.push((function, cfg));
        }

        if functions.is_empty() {
            return Ok(PreservedAnalyses::All);
        }

        for (function, cfg) in functions {
            let mut algo: Box<dyn AliasAccessAlgo> = match cfg.mode {
                AliasAccessMode::PointerChain => Box::new(PointerChainAlgo::default()),
            };

            if let Err(e) = algo.initialize(&cfg) {
                error!("failed to initialize: {}", e);
                return Ok(PreservedAnalyses::All);
            }

            if let Err(e) = algo.do_alias_access(&cfg, module, function) {
                error!("failed to do alias access: {}", e);
                return Ok(PreservedAnalyses::All);
            }

            if let VerifyResult::Broken(e) = function.verify_function() {
                warn!("function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

pub(crate) trait AliasAccessAlgo {
    fn initialize(&mut self, pass: &AliasAccessConfig) -> anyhow::Result<()>;

    fn do_alias_access(
        &mut self,
        pass: &AliasAccessConfig,
        module: &Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()>;
}

mod basic;
mod polaris_primes;

use crate::aotu::bogus_control_flow::basic::BogusControlFlowBasic;
use crate::aotu::bogus_control_flow::polaris_primes::BogusControlFlowPolarisPrimes;
use crate::config::{BogusControlFlowConfig, BogusControlFlowMode, Config};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, VerifyResult};
use amice_macro::amice;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;

#[amice(
    priority = 950,
    name = "BogusControlFlow",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = BogusControlFlowConfig,
)]
#[derive(Default)]
pub struct BogusControlFlow {}

impl AmicePass for BogusControlFlow {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.bogus_control_flow.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut executed = false;
        for function in module.get_functions() {
            if function.is_inline_marked() || function.is_llvm_function() || function.is_undef_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;

            if !cfg.enable {
                continue;
            }

            let mut algo: Box<dyn BogusControlFlowAlgo> = match cfg.mode {
                BogusControlFlowMode::Basic => Box::new(BogusControlFlowBasic::default()),
                BogusControlFlowMode::PolarisPrimes => Box::new(BogusControlFlowPolarisPrimes::default()),
            };

            if let Err(err) = algo.initialize(&cfg, module) {
                error!("initialize failed: {}", err);
                continue;
            }

            if let Err(err) = algo.apply_bogus_control_flow(&cfg, module, function) {
                error!("apply failed: {}", err);
                continue;
            }

            executed = true;
            if let VerifyResult::Broken(msg) = function.verify_function() {
                error!("function {:?} is broken: {}", function.get_name(), msg);
            }
        }

        if !executed {
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

pub(super) trait BogusControlFlowAlgo {
    fn initialize(&mut self, cfg: &BogusControlFlowConfig, module: &mut Module<'_>) -> anyhow::Result<()>;

    fn apply_bogus_control_flow(
        &mut self,
        cfg: &BogusControlFlowConfig,
        module: &mut Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()>;
}

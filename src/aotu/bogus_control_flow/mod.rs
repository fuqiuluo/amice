mod basic;
mod polaris_primes;

use crate::aotu::bogus_control_flow::basic::BogusControlFlowBasic;
use crate::aotu::bogus_control_flow::polaris_primes::BogusControlFlowPolarisPrimes;
use crate::config::{BogusControlFlowMode, Config};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{FunctionExt, VerifyResult};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error};

#[amice(priority = 950, name = "BogusControlFlow", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct BogusControlFlow {
    enable: bool,
    mode: BogusControlFlowMode,
    probability: u32,
    loop_count: u32,
}

impl AmicePassLoadable for BogusControlFlow {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.bogus_control_flow.enable;

        self.mode = cfg.bogus_control_flow.mode;
        self.probability = cfg.bogus_control_flow.probability;
        self.loop_count = cfg.bogus_control_flow.loop_count;

        if self.enable {
            debug!(
                "BogusControlFlow pass enabled with probability: {}%, loops: {}",
                self.probability, self.loop_count
            );
        }

        self.enable
    }
}

impl LlvmModulePass for BogusControlFlow {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mut algo: Box<dyn BogusControlFlowAlgo> = match self.mode {
            BogusControlFlowMode::Basic => Box::new(BogusControlFlowBasic::default()),
            BogusControlFlowMode::PolarisPrimes => Box::new(BogusControlFlowPolarisPrimes::default()),
        };

        if let Err(err) = algo.initialize(self, module) {
            error!("(bogus-control-flow) initialize failed: {}", err);
            return PreservedAnalyses::All;
        }

        if let Err(err) = algo.apply_bogus_control_flow(self, module) {
            error!("(bogus-control-flow) apply failed: {}", err);
            return PreservedAnalyses::All;
        }

        for x in module.get_functions() {
            if let VerifyResult::Broken(msg) = x.verify_function() {
                error!("(bogus-control-flow) function {:?} is broken: {}", x.get_name(), msg);
            }
        }

        PreservedAnalyses::None
    }
}

pub(super) trait BogusControlFlowAlgo {
    fn initialize(&mut self, pass: &BogusControlFlow, module: &mut Module<'_>) -> anyhow::Result<()>;

    fn apply_bogus_control_flow(&mut self, pass: &BogusControlFlow, module: &mut Module<'_>) -> anyhow::Result<()>;
}

use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::FunctionExt;
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, log_enabled, warn};

#[amice(priority = 1120, name = "ParamAggregate", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct ParamAggregate {
    enable: bool,
}

impl AmicePassLoadable for ParamAggregate {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.param_aggregate.enable;

        self.enable
    }
}

impl LlvmModulePass for ParamAggregate {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if function.is_inline_marked() || function.is_llvm_function() {
                continue;
            }

            if let Err(e) = handle_function(function) {
                error!(
                    "(param-aggregate) failed to handle function {:?}: {}",
                    function.get_name(),
                    e
                );
            }
        }

        PreservedAnalyses::None
    }
}

fn handle_function(function: FunctionValue<'_>) -> anyhow::Result<()> {
    Ok(())
}

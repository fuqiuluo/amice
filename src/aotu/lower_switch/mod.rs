use crate::config::Config;
use crate::pass_registry::AmicePassLoadable;
use amice_llvm::module_utils::verify_function;
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{error, warn};

#[amice(priority = 961, name = "LowerSwitch")]
#[derive(Default)]
pub struct LowerSwitch {
    enable: bool,
}

impl AmicePassLoadable for LowerSwitch {
    fn init(&mut self, cfg: &Config) -> bool {
        self.enable = cfg.lower_switch.enable;

        self.enable
    }
}

impl LlvmModulePass for LowerSwitch {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(e) = do_lower_switch(module, function) {
                error!(
                    "Failed to lower switch in function {}: {}",
                    function.get_name().to_str().unwrap_or("unknown"),
                    e
                );
            }
        }

        for f in module.get_functions() {
            if verify_function(f.as_value_ref() as *mut std::ffi::c_void) {
                warn!("(lower-switch) function {:?} is not verified", f.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

fn do_lower_switch(module: &mut Module<'_>, function: FunctionValue) -> anyhow::Result<()> {
    Ok(())
}

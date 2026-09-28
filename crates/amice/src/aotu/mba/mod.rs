//! Bounded SSA representation changes, with an integer-only fallback.
mod algebra;
mod integer;
mod region;

use crate::config::{Config, MbaConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, ModuleExt};
use amice_macro::amice;
use amice_plugin::inkwell::attributes::AttributeLoc;
use amice_plugin::{PreservedAnalyses, inkwell::module::Module};

#[amice(
    priority = 955,
    name = "Mba",
    flag = AmicePassFlag::OptimizerLast | AmicePassFlag::FunctionLevel,
    config = MbaConfig,
)]
#[derive(Default)]
pub struct Mba {}

impl AmicePass for Mba {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.mba.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut changed = false;
        let mut changed_effects = false;
        for function in module.get_functions() {
            if function.is_undef_function()
                || function.is_llvm_function()
                || function
                    .get_string_attribute(AttributeLoc::Function, "amice.mba.done")
                    .is_some()
            {
                continue;
            }
            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }
            let (rewritten, effects) = region::rewrite(module, function, &cfg)?;
            changed_effects |= effects;
            if rewritten {
                let attr = module.get_context().create_string_attribute("amice.mba.done", "1");
                function.add_attribute(AttributeLoc::Function, attr);
                changed = true;
            }
        }
        if changed_effects {
            // Caller summaries and explicit call-site attributes can retain
            // memory(none), including through indirect calls and aliases. Both
            // volatile guards and constrained FP access observable state.
            module.invalidate_memory_attrs_for_volatile();
        }
        Ok(if changed {
            PreservedAnalyses::None
        } else {
            PreservedAnalyses::All
        })
    }
}

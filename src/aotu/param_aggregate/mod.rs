use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{AttributeEnumKind, FunctionExt, ModuleExt, VerifyResult};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::llvm_sys::core::LLVMGetEnumAttributeKind;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FIRST_CUSTOM_METADATA_KIND_ID, FunctionValue};
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

        #[cfg(feature = "android-ndk")]
        return false;

        self.enable
    }
}

impl LlvmModulePass for ParamAggregate {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        #[cfg(not(feature = "android-ndk"))]
        {
            let mut functions = Vec::new();
            'out: for function in module.get_functions() {
                if function.is_inline_marked() || function.is_llvm_function() || function.is_undef_function() {
                    continue;
                }

                if function.count_params() <= 1 {
                    continue;
                }

                for i in 0..function.count_params() {
                    // https://github.com/llvm/llvm-project/blob/main/llvm/include/llvm/IR/Attributes.td
                    let param_attrs = function.attributes(AttributeLoc::Param(i));
                    for x in param_attrs {
                        if x.is_string() || x.is_type() {
                            continue 'out;
                        }

                        // enum kind 可以选择性的跳过
                        let enum_kind = match AttributeEnumKind::from_raw(x.get_enum_kind_id()) {
                            Ok(kind) => kind,
                            Err(_) => {
                                error!(
                                    "(param-aggregate) failed parse enum kind id, name = {}",
                                    AttributeEnumKind::get_raw_name(x.get_enum_kind_id())
                                );
                                continue 'out;
                            },
                        };
                    }
                }

                functions.push(function);
            }

            for function in functions {
                match handle_function(module, function) {
                    Err(e) => {
                        error!("(param-aggregate) failed handle_function {}", e);
                    },
                    Ok(new_function) => {
                        if let VerifyResult::Broken(errmsg) = new_function.verify_function() {
                            error!("(param-aggregate) failed verify_function {}", errmsg);
                        }
                    },
                }
            }
        }

        #[cfg(feature = "android-ndk")]
        {
            error!("(param-aggregate) not support android-ndk");
        }

        PreservedAnalyses::None
    }
}

#[cfg(not(feature = "android-ndk"))]
fn handle_function<'a>(module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<FunctionValue<'a>> {
    let cloned_function = unsafe { module.specialize_function_by_args(function, &[]) }
        .map_err(|e| anyhow!("(param-aggregate) function_specialize_partial failed: {}", e))?;

    debug!("(param-aggregate) handle function: {:?}", function.get_name());

    Ok(cloned_function)
}

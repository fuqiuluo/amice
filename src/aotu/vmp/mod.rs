mod avm;
mod compiler;
mod translator;

use crate::aotu::vmp::compiler::AVMCompilerContext;
use crate::aotu::vmp::translator::IRConverter;
use crate::config::{Config, VMPConfig, VMPFlag};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, InstructionExt};
use amice_macro::amice;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};

#[amice(
    priority = 800,
    name = "VMP",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = VMPConfig,
)]
#[derive(Default)]
pub struct VMP {}

impl AmicePass for VMP {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.vmp.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_undef_function() || function.is_llvm_function() || function.is_inline_marked() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            functions.push(function);
        }

        for x in functions {
            if let Err(e) = do_handle_function(module, x, self.default_config.flags) {
                error!("Failed to apply VMP to function {:?}: {}", x.get_name(), e);
            }
        }

        Ok(PreservedAnalyses::All)
    }
}

fn do_handle_function(module: &mut Module<'_>, function: FunctionValue, flags: VMPFlag) -> anyhow::Result<()> {
    let mut context = AVMCompilerContext::new(function, flags)?;
    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            context.translate(inst)?;
        }
    }

    debug!(
        "Function {:?} converted to AVM opcodes: {}",
        function.get_name(),
        context
    );

    Ok(())
}

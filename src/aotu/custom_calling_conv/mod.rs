use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::annotate::read_annotate;
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, CallSiteValue, FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::error;

#[amice(priority = 1121, name = "CustomCallingConv", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct CustomCallingConv {
    enable: bool,
}

impl AmicePassLoadable for CustomCallingConv {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.custom_calling_conv.enable;

        self.enable
    }
}

impl LlvmModulePass for CustomCallingConv {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::None;
        }

        for function in module.get_functions() {
            if function.get_intrinsic_id() != 0 {
                continue;
            }

            if let Err(e) = do_random_calling_conv(module, function) {
                error!(
                    "(custom-calling-conv) failed to process function {:?}: {}",
                    function.get_name(),
                    e
                );
            }
        }

        PreservedAnalyses::None
    }
}

fn do_random_calling_conv<'a>(module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<()> {
    let annotates = read_annotate(module, function).map_err(|e| anyhow::anyhow!("failed to read annotate: {}", e))?;

    if !annotates.iter().any(|annotate| {
        annotate == "random_calling_conv"
            || annotate == "custom_calling_conv"
            || annotate.contains("customcc")
    }) {
        return Ok(());
    }

    let obf_calling_conv = [0u32; 0]; // todo: 等待CodeGen部分实现hook以提供自定义CallingConv支持
    if obf_calling_conv.is_empty() {
        return Ok(());
    }

    let random_calling_conv = obf_calling_conv[rand::random_range(0..obf_calling_conv.len())];
    function.set_call_conventions(random_calling_conv);

    for func in module.get_functions() {
        for bb in func.get_basic_blocks() {
            for inst in bb.get_instructions() {
                unsafe {
                    if inst.get_opcode() == InstructionOpcode::Call {
                        let call_site = CallSiteValue::new(inst.as_value_ref());
                        call_site.set_call_convention(random_calling_conv)
                    }
                }
            }
        }
    }

    Ok(())
}

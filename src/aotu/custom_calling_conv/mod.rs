use crate::config::{Config, CustomCallingConvConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, ModuleExt};
use amice_macro::amice;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, CallSiteValue, FunctionValue, InstructionOpcode};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(
    priority = 1121,
    name = "CustomCallingConv",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = CustomCallingConvConfig,
)]
#[derive(Default)]
pub struct CustomCallingConv {}

impl AmicePass for CustomCallingConv {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.custom_calling_conv.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut executed = false;
        for function in module.get_functions() {
            if function.is_llvm_function() || function.is_undef_function() || function.is_inline_marked() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            if let Err(e) = do_random_calling_conv(module, function) {
                error!("failed to process function {:?}: {}", function.get_name(), e);
                continue;
            }

            executed = true;
        }

        if !executed {
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

fn do_random_calling_conv<'a>(module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<()> {
    let annotates = module
        .read_function_annotate(function)
        .map_err(|e| anyhow::anyhow!("failed to read annotate: {}", e))?;

    if !annotates.iter().any(|annotate| {
        annotate == "+random_calling_conv" || annotate == "+custom_calling_conv" || annotate.contains("+customcc")
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

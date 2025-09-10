mod bytecode;
mod codegen;
mod compiler;
mod isa;
mod runtime;
mod translator;

use crate::aotu::vmp::codegen::VMPCodeGenerator;
use crate::aotu::vmp::compiler::VMPCompilerContext;
use crate::aotu::vmp::runtime::VMPRuntime;
use crate::aotu::vmp::translator::IRConverter;
use crate::config::{Config, VMPConfig, VMPFlag};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, InstructionExt};
use amice_macro::amice;
use llvm_plugin::PreservedAnalyses;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode};
use log::{Level, log_enabled};

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
        info!("Starting VMP transformation...");

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

        let mut codegen = VMPCodeGenerator::new(module)?;

        // 处理每个函数
        for function in functions {
            if let Err(e) = self.handle_function_with_vm(function, &mut codegen, self.default_config.flags) {
                error!("failed to apply VMP to function {:?}: {}", function.get_name(), e);
            } else {
                info!("successfully applied VMP to function {:?}", function.get_name());
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

impl VMP {
    /// 使用虚拟机方式处理函数
    fn handle_function_with_vm(
        &self,
        function: FunctionValue,
        codegen: &mut VMPCodeGenerator,
        flags: VMPFlag,
    ) -> anyhow::Result<()> {
        if log_enabled!(Level::Debug) {
            debug!("translating function {:?} to VM instructions", function.get_name());
        }

        let context = self.translate_function_to_vm(function, flags)?;

        // let mut runtime = JustAVMRuntime::new();
        // let result = runtime.execute(&vm_instructions);
        // debug!("return => {:?}", result);

        // 将AVM指令编译为调用虚拟机运行时的LLVM IR
        codegen.compile_function_to_vm_call(function, context)?;

        Ok(())
    }

    /// 将函数翻译为虚拟机指令序列
    fn translate_function_to_vm(&self, function: FunctionValue, flags: VMPFlag) -> anyhow::Result<VMPCompilerContext> {
        let mut context = VMPCompilerContext::new(function, flags)?;

        if function.count_params() > 0 {
            if log_enabled!(Level::Debug) {
                debug!("Function has {} parameters", function.count_params());
            }

            for (i, param) in function.get_param_iter().enumerate() {
                let param_reg = context.get_or_allocate_register(param.as_value_ref() as LLVMValueRef, true);
                if log_enabled!(Level::Debug) {
                    debug!("Allocated register {} for parameter {}", param_reg.value, i);
                }
            }
        }

        for bb in function.get_basic_blocks() {
            if log_enabled!(Level::Debug) {
                debug!(
                    "Processing basic block with {} instructions",
                    bb.get_instructions().count()
                );
            }

            for inst in bb.get_instructions() {
                if let Err(e) = context.translate(inst) {
                    error!("Failed to translate instruction {:?}: {}", inst, e);
                    return Err(e);
                }
            }
        }

        Ok(context)
    }
}

/// 检查函数是否适合虚拟化
fn is_function_suitable_for_virtualization(function: FunctionValue) -> bool {
    // 检查函数复杂度、是否包含不支持的指令等
    let mut instruction_count = 0;
    let mut has_unsupported_instructions = false;

    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            instruction_count += 1;

            // 检查是否有不支持的指令
            match inst.get_opcode() {
                InstructionOpcode::IndirectBr
                | InstructionOpcode::Invoke
                | InstructionOpcode::CallBr
                | InstructionOpcode::Resume
                | InstructionOpcode::CatchPad
                | InstructionOpcode::CatchRet
                | InstructionOpcode::CatchSwitch
                | InstructionOpcode::CleanupPad
                | InstructionOpcode::CleanupRet => {
                    has_unsupported_instructions = true;
                    break;
                },
                _ => {},
            }
        }

        if has_unsupported_instructions {
            break;
        }
    }

    // 函数不能太小（没有虚拟化的价值）也不能太大（编译时间过长）
    const MIN_INSTRUCTIONS: usize = 1;
    const MAX_INSTRUCTIONS: usize = 1000;

    !has_unsupported_instructions && instruction_count >= MIN_INSTRUCTIONS && instruction_count <= MAX_INSTRUCTIONS
}

/// 统计信息结构
#[derive(Debug, Default)]
pub struct VMPStatistics {
    pub functions_processed: usize,
    pub functions_virtualized: usize,
    pub functions_skipped: usize,
    pub total_instructions_translated: usize,
    pub total_vm_instructions_generated: usize,
}

impl VMPStatistics {
    pub fn print_summary(&self) {
        info!("VMP Transformation Summary:");
        info!("  Functions processed: {}", self.functions_processed);
        info!("  Functions virtualized: {}", self.functions_virtualized);
        info!("  Functions skipped: {}", self.functions_skipped);
        info!(
            "  Total LLVM instructions translated: {}",
            self.total_instructions_translated
        );
        info!(
            "  Total VM instructions generated: {}",
            self.total_vm_instructions_generated
        );

        if self.functions_processed > 0 {
            let virtualization_rate = (self.functions_virtualized as f64 / self.functions_processed as f64) * 100.0;
            info!("  Virtualization rate: {:.2}%", virtualization_rate);
        }

        if self.total_instructions_translated > 0 {
            let expansion_ratio =
                self.total_vm_instructions_generated as f64 / self.total_instructions_translated as f64;
            info!("  Instruction expansion ratio: {:.2}x", expansion_ratio);
        }
    }
}

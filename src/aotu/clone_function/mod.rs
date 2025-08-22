use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::{Config, FlattenMode, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::basic_block::split_basic_block;
use amice_llvm::ir::function::{fix_stack, is_inline_marked_function};
use amice_llvm::module_utils::{VerifyResult, verify_function, verify_function2};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, IntValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use rand::Rng;
use std::collections::HashMap;

#[amice(priority = 1111, name = "CloneFunction", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct CloneFunction {
    enable: bool,
}

impl AmicePassLoadable for CloneFunction {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.clone_function.enable;

        self.enable
    }
}

impl LlvmModulePass for CloneFunction {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        let mut call_instructions = Vec::new();
        for function in module.get_functions() {
            if function.count_basic_blocks() == 0 {
                continue;
            }

            // Check if function should be obfuscated (using similar logic from other passes)
            let function_name = function.get_name().to_str().unwrap_or("");
            if should_skip_function_by_name(function_name)
                || should_skip_function_by_inline_attribute(function)
            {
                continue;
            }

            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if matches!(inst.get_opcode(), InstructionOpcode::Call) {
                        if let Some(called_func) = get_called_function(&inst) {

                            let function_name = called_func.get_name().to_str().unwrap_or("");
                            if should_skip_function_by_name(function_name)
                                || should_skip_function_by_inline_attribute(called_func)
                                || should_skip_function_by_defined_state(called_func)
                            {
                                continue;
                            }

                            debug!("(clone-function) adding call to function: {:?}",called_func.get_name());
                            call_instructions.push((inst, called_func));
                        }
                    }
                }
            }
        }



        PreservedAnalyses::None
    }
}

/// Check if a function should be skipped from obfuscation
fn should_skip_function_by_name(name: &str) -> bool {
    // Skip intrinsics, compiler-generated functions, and system functions
    name.starts_with("llvm.")
        || name.starts_with("clang.")
        || name.starts_with("__")
        || name.starts_with("@")
        || name.is_empty()
}

fn should_skip_function_by_inline_attribute(function_value: FunctionValue) -> bool {
    is_inline_marked_function(function_value)
}

fn should_skip_function_by_defined_state(function_value: FunctionValue) -> bool {
    function_value.is_null() || function_value.is_undef() || function_value.count_basic_blocks() <= 0
}

fn get_called_function<'a>(inst: &InstructionValue<'a>) -> Option<FunctionValue<'a>> {
    // %call38 = call i32 (ptr, ...) @printf(ptr noundef @.str.18, i32 noundef %18, i32 noundef %19)
    match inst.get_opcode() {
        InstructionOpcode::Call => {
            let operand_num = inst.get_num_operands();
            if operand_num == 0 {
                return None;
            }

            // for x in inst.get_operands() {
            //     debug!("Operand: {:?}", x);
            // }

            // The last operand of a call instruction is typically the called function
            if let Some(operand) = inst.get_operand(operand_num - 1) {
                if let Some(callee) = operand.left() {
                    let callee_ptr = callee.into_pointer_value();
                    if let Some(func_val) = unsafe { FunctionValue::new(callee_ptr.as_value_ref()) } {
                        return Some(func_val);
                    }
                }
            }
            None
        },
        _ => None,
    }
}
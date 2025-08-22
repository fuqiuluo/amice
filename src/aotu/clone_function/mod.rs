use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::{Config, FlattenMode, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::basic_block::split_basic_block;
use amice_llvm::ir::function::{fix_stack, function_specialize_partial, is_inline_marked_function};
use amice_llvm::module_utils::{VerifyResult, verify_function, verify_function2};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMModuleRef, LLVMValueRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue,
    IntValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use rand::Rng;
use std::collections::{BTreeSet, HashMap};
use std::result;
use llvm_plugin::inkwell::llvm_sys::core::LLVMIsAIntrinsicInst;

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
            if should_skip_function_by_name(function_name) || should_skip_function_by_inline_attribute(function) {
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

                            //debug!("(clone-function) adding call to function: {:?}",called_func.get_name());
                            call_instructions.push((inst, called_func));
                        }
                    }
                }
            }
        }

        let mut call_instructions_with_constant_args = Vec::new();
        for (call, call_func) in call_instructions {
            let mut args = Vec::new();
            for i in 0..call.get_num_operands() {
                let operand = call.get_operand(i);
                if let Some(operand) = operand
                    && let Some(operand_value) = operand.left()
                    && (operand_value.is_int_value() || operand_value.is_float_value())
                {
                    let is_const = match operand_value {
                        BasicValueEnum::IntValue(iv) => iv.is_const(),
                        BasicValueEnum::FloatValue(fv) => fv.is_const(),
                        _ => false,
                    };
                    if is_const {
                        args.push((i as u32, operand_value));
                    }
                }
            }
            if args.len() > 0 {
                //debug!("(clone-function) adding call to function: {:?}",call_func.get_name());
                call_instructions_with_constant_args.push((call, call_func, args));
            }
        }

        if call_instructions_with_constant_args.is_empty() {
            return PreservedAnalyses::All;
        }

        // pub unsafe fn function_specialize_partial(
        //     module: LLVMModuleRef,
        //     original_func: LLVMValueRef,
        //     replacements: &[(u32, LLVMValueRef)],
        // ) -> Result<LLVMValueRef, &'static str>
        for (inst, call_func, args) in call_instructions_with_constant_args {
            if let Err(e) = do_replace_call_with_call_to_specialized_function(module, inst, call_func, args) {
                error!(
                    "(clone-function) failed to replace call with specialized function: {}",
                    e
                );
            }
        }

        PreservedAnalyses::None
    }
}

fn do_replace_call_with_call_to_specialized_function(
    module: &mut Module<'_>,
    call_inst: InstructionValue<'_>,
    call_func: FunctionValue<'_>,
    args: Vec<(u32, BasicValueEnum)>,
) -> anyhow::Result<()> {
    let replacements = args
        .iter()
        .map(|(i, operand)| (*i, operand.as_value_ref() as LLVMValueRef))
        .collect::<Vec<(u32, LLVMValueRef)>>();
    let special_function = unsafe {
        function_specialize_partial(
            module.as_mut_ptr() as LLVMModuleRef,
            call_func.as_value_ref() as LLVMValueRef,
            &replacements,
        )
    }
    .map_err(|e| anyhow!("(clone-function) function_specialize_partial failed: {}", e))?;

    let special_function = unsafe { FunctionValue::new(special_function) }
        .ok_or_else(|| anyhow!("(clone-function) failed to create FunctionValue from specialized function"))?;

    let context = module.get_context();
    let builder = context.create_builder();
    builder.position_before(&call_inst);

    // 原调用的参数个数（不含最后一个被调函数操作数）
    let total_operands = call_inst.get_num_operands();
    if total_operands == 0 {
        return Err(anyhow!("(clone-function) call has no operands"));
    }
    let callee_operand_index = total_operands - 1;

    // 将被特化（替换）的参数索引放入集合，便于判断
    let mut replaced_idx = BTreeSet::new();
    for (idx, _) in &args {
        replaced_idx.insert(*idx);
    }

    // 构造传给特化后函数的参数：仅保留未被替换的参数，按原顺序
    let mut new_call_args: Vec<BasicMetadataValueEnum> = Vec::new();
    for i in 0..callee_operand_index {
        if replaced_idx.contains(&i) {
            continue;
        }
        if let Some(op) = call_inst.get_operand(i) {
            if let Some(val) = op.left() {
                new_call_args.push(val.into());
            } else {
                return Err(anyhow!("(clone-function) operand {} of call is not a value", i));
            }
        } else {
            return Err(anyhow!("(clone-function) missing operand {} for original call", i));
        }
    }

    // 生成新的调用指令
    let new_call_site = builder.build_call(special_function, &new_call_args, "cloned.call")?;

    let new_inst = unsafe { InstructionValue::new(new_call_site.as_value_ref()) };

    // 如果原调用有返回值，则替换所有 uses
    let is_void_ret = call_inst.get_type().is_void_type();
    if !is_void_ret {
        call_inst.replace_all_uses_with(&new_inst);
    }

    // 删除旧调用
    call_inst.erase_from_basic_block();

    Ok(())
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
    function_value.is_null()
        || function_value.is_undef()
        || function_value.count_basic_blocks() <= 0
        || function_value.get_intrinsic_id() != 0
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

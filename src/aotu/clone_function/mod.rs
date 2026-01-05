use crate::config::{CloneFunctionConfig, Config};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, LLVMValueRefExt, ModuleExt};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::attributes::AttributeLoc;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMModuleRef, LLVMValueRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use std::collections::BTreeSet;

#[amice(
    priority = 1111,
    name = "CloneFunction",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = CloneFunctionConfig,
)]
#[derive(Default)]
pub struct CloneFunction {}

impl AmicePass for CloneFunction {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.clone_function.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        debug!("default_config.enable = {}", self.default_config.enable);

        if !self.default_config.enable {
            return Ok(PreservedAnalyses::All);
        }

        #[cfg(feature = "android-ndk")]
        {
            error!("clone-function pass is not supported on android-ndk");
            return Ok(PreservedAnalyses::All);
        }

        #[cfg(not(feature = "android-ndk"))]
        {
            let mut call_instructions = Vec::new();
            let mut func_count = 0;
            for function in module.get_functions() {
                if function.is_llvm_function() || function.is_undef_function() {
                    continue;
                }
                func_count += 1;

                let cfg = self.parse_function_annotations(module, function)?;
                if !cfg.enable {
                    debug!("function {:?} skipped: enable=false", function.get_name());
                    continue;
                }

                for bb in function.get_basic_blocks() {
                    for inst in bb.get_instructions() {
                        if matches!(inst.get_opcode(), InstructionOpcode::Call) {
                            if let Some(called_func) = get_called_function(&inst) {
                                if called_func.is_llvm_function() {
                                    debug!("skip llvm function call: {:?}", called_func.get_name());
                                    continue;
                                }
                                if called_func.is_undef_function() {
                                    debug!("skip undef function call: {:?}", called_func.get_name());
                                    continue;
                                }
                                if called_func.is_inline_marked() {
                                    debug!("skip inline function call: {:?}", called_func.get_name());
                                    continue;
                                }

                                if called_func.get_type().is_var_arg() {
                                    debug!("skipping varargs function: {:?}", called_func.get_name());
                                    continue;
                                }

                                //debug!("(clone-function) adding call to function: {:?}",called_func.get_name());
                                call_instructions.push((inst, called_func));
                            }
                        }
                    }
                }
            }

            if call_instructions.is_empty() {
                debug!("no call instructions found, func_count={}", func_count);
                return Ok(PreservedAnalyses::All);
            }

            let mut call_instructions_with_constant_args = Vec::new();
            for (call, call_func) in call_instructions {
                let mut args = Vec::new();
                for i in 0..call.get_num_operands() {
                    let operand = call.get_operand(i);
                    if let Some(operand) = operand
                        && let Some(operand_value) = operand.value()
                        && (operand_value.is_int_value() || operand_value.is_float_value())
                    {
                        let is_const = match operand_value {
                            BasicValueEnum::IntValue(iv) => iv.is_const(),
                            BasicValueEnum::FloatValue(fv) => fv.is_const(),
                            _ => false,
                        };
                        if is_const {
                            args.push((i, operand_value));
                        }
                    }
                }
                if args.len() > 0 {
                    //debug!("(clone-function) adding call to function: {:?}",call_func.get_name());
                    call_instructions_with_constant_args.push((call, call_func, args));
                }
            }

            if call_instructions_with_constant_args.is_empty() {
                debug!("no call instructions with constant args found");
                return Ok(PreservedAnalyses::All);
            }

            debug!(
                "found {} call sites with constant args",
                call_instructions_with_constant_args.len()
            );

            for (inst, call_func, args) in call_instructions_with_constant_args {
                debug!(
                    "specializing function {:?} with {} constant args",
                    call_func.get_name(),
                    args.len()
                );
                if let Err(e) = do_replace_call_with_call_to_specialized_function(module, inst, call_func, args) {
                    error!("failed to replace call with specialized function: {}", e);
                }
            }

            return Ok(PreservedAnalyses::None);
        }
    }
}

#[cfg(not(feature = "android-ndk"))]
fn do_replace_call_with_call_to_specialized_function<'a>(
    module: &mut Module<'a>,
    call_inst: InstructionValue<'_>,
    call_func: FunctionValue<'a>,
    args: Vec<(u32, BasicValueEnum)>,
) -> anyhow::Result<()> {
    let replacements = args
        .iter()
        .map(|(i, operand)| (*i, operand.as_value_ref() as LLVMValueRef))
        .collect::<Vec<(u32, LLVMValueRef)>>();
    let special_function = unsafe { module.specialize_function_by_args(call_func, &replacements) }
        .map_err(|e| anyhow!("function_specialize_partial failed: {}", e))?;

    for (arg_index, _) in replacements {
        for attr in special_function.attributes(AttributeLoc::Param(arg_index)) {
            if attr.is_enum() {
                special_function.remove_enum_attribute(AttributeLoc::Param(arg_index), attr.get_enum_kind_id())
            } else if attr.is_string() {
                special_function
                    .remove_string_attribute(AttributeLoc::Param(arg_index), attr.get_string_kind_id().to_str()?)
            }
        }
    }

    let context = module.get_context();
    let builder = context.create_builder();
    builder.position_before(&call_inst);

    // 原调用的参数个数（不含最后一个被调函数操作数）
    let total_operands = call_inst.get_num_operands();
    if total_operands == 0 {
        return Err(anyhow!("call has no operands"));
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
            if let Some(val) = op.value() {
                new_call_args.push(val.into());
            } else {
                return Err(anyhow!("operand {} of call is not a value", i));
            }
        } else {
            return Err(anyhow!("missing operand {} for original call", i));
        }
    }

    // 生成新的调用指令
    let new_call_site = builder.build_call(special_function, &new_call_args, "cloned.call")?;

    let new_inst = (new_call_site.as_value_ref() as LLVMValueRef).into_instruction_value();

    // 如果原调用有返回值，则替换所有 uses
    let is_void_ret = call_inst.get_type().is_void_type();
    if !is_void_ret {
        call_inst.replace_all_uses_with(&new_inst);
    }

    // 删除旧调用
    call_inst.erase_from_basic_block();

    Ok(())
}

fn get_called_function<'a>(inst: &InstructionValue<'a>) -> Option<FunctionValue<'a>> {
    // %call38 = call i32 (ptr, ...) @printf(ptr noundef @.str.18, i32 noundef %18, i32 noundef %19)
    match inst.get_opcode() {
        InstructionOpcode::Call => {
            let operand_num = inst.get_num_operands();
            if operand_num == 0 {
                return None;
            }

            // The last operand of a call instruction is typically the called function
            if let Some(operand) = inst.get_operand(operand_num - 1) {
                if let Some(callee) = operand.value() {
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

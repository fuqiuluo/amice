use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{FunctionExt, InstructionExt, LLVMValueRefExt, ModuleExt};
use amice_macro::amice;
use llvm_plugin::inkwell::attributes::AttributeLoc;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::BasicTypeEnum;
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, FunctionValue, InstructionOpcode, InstructionValue,
};
use llvm_plugin::inkwell::{Either::Left, Either::Right, context::ContextRef};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error};
use rand::Rng;

#[amice(priority = 1010, name = "FunctionWrapper", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct FunctionWrapper {
    enable: bool,
    probability: u32,
    times: u32,
}

impl AmicePassLoadable for FunctionWrapper {
    fn init(&mut self, cfg: &Config, _position: PassPosition) -> bool {
        self.enable = cfg.function_wrapper.enable;
        self.probability = cfg.function_wrapper.probability.min(100);
        self.times = cfg.function_wrapper.times.max(1);

        // debug!(
        //     "(function-wrapper) initialized with enable={}, probability={}%, times={}",
        //     self.enable, self.probability, self.times
        // );

        self.enable
    }
}

impl LlvmModulePass for FunctionWrapper {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        debug!("(function-wrapper) starting function wrapper pass");

        // Collect call sites that need to be wrapped
        let mut call_instructions = Vec::new();
        for function in module.get_functions() {
            if function.count_basic_blocks() == 0 {
                continue;
            }

            if function.is_llvm_function() || function.is_inline_marked() {
                continue;
            }

            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if matches!(inst.get_opcode(), InstructionOpcode::Call | InstructionOpcode::Invoke) {
                        // Apply probability check
                        if rand::random_range(0..100) < self.probability {
                            if let Some(called_func) = get_called_function(&inst) {
                                debug!(
                                    "(function-wrapper) adding call to function: {:?}",
                                    called_func.get_name()
                                );
                                call_instructions.push((inst, called_func));
                            }
                        }
                    }
                }
            }
        }

        debug!(
            "(function-wrapper) collected {} call sites for wrapping",
            call_instructions.len()
        );

        // Apply wrapper transformation multiple times
        let mut current_call_instructions = call_instructions;
        for iteration in 0..self.times {
            debug!(
                "(function-wrapper) applying wrapper iteration {}/{}",
                iteration + 1,
                self.times
            );

            let mut next_call_instructions = Vec::new();
            for (inst, called_func) in current_call_instructions {
                match handle_call_instruction(module, inst, Some(called_func)) {
                    Ok(Some(new_inst)) => {
                        if let Some(new_called_func) = get_called_function(&new_inst) {
                            next_call_instructions.push((new_inst, new_called_func));
                        }
                    },
                    Ok(None) => {
                        debug!("(function-wrapper) call instruction was not wrapped (filtered out)");
                    },
                    Err(e) => {
                        error!("(function-wrapper) failed to handle call instruction: {}", e);
                    },
                }
            }
            current_call_instructions = next_call_instructions;
        }

        debug!("(function-wrapper) completed function wrapper pass");
        PreservedAnalyses::None
    }
}

/// Extract called function from a call instruction
fn get_called_function<'a>(inst: &InstructionValue<'a>) -> Option<FunctionValue<'a>> {
    match inst.get_opcode() {
        InstructionOpcode::Call => inst.into_call_inst().get_call_function(),
        InstructionOpcode::Invoke => {
            let operand_num = inst.get_num_operands();
            if operand_num == 0 {
                return None;
            }

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

/// Handle a single call instruction by creating a wrapper function
fn handle_call_instruction<'a>(
    module: &mut Module<'a>,
    call_inst: InstructionValue<'a>,
    called_function: Option<FunctionValue<'a>>,
) -> anyhow::Result<Option<InstructionValue<'a>>> {
    let Some(called_function) = called_function else {
        debug!("(function-wrapper) skipping call with no function");
        return Ok(None);
    };

    // Skip intrinsic functions
    if called_function.get_intrinsic_id() != 0 {
        debug!("(function-wrapper) skipping intrinsic function");
        return Ok(None);
    }

    if called_function.is_llvm_function() || called_function.is_inline_marked() {
        return Ok(None);
    }

    // Create wrapper function
    create_wrapper_function(module, &call_inst, called_function)
}

/// Create a wrapper function for the given call instruction
fn create_wrapper_function<'a>(
    module: &mut Module<'a>,
    call_inst: &InstructionValue<'a>,
    called_function: FunctionValue<'a>,
) -> anyhow::Result<Option<InstructionValue<'a>>> {
    let ctx = module.get_context();

    // Use the called function's type directly for the wrapper
    let called_fn_type = called_function.get_type();

    // Generate random wrapper function name
    let wrapper_name = generate_wrapper_name();

    // Create the wrapper function with the same signature as the called function
    let wrapper_function = module.add_function(&wrapper_name, called_fn_type, Some(Linkage::Internal));

    // Copy some attributes from the original function (simplified)
    copy_function_attributes(&wrapper_function, &called_function);

    // Mark as used to prevent elimination
    module.append_to_compiler_used(wrapper_function.as_global_value());

    // Create the wrapper function body
    create_wrapper_body(ctx, &wrapper_function, &called_function)?;

    // Replace the call instruction to use the wrapper function
    replace_call_with_wrapper(call_inst, &wrapper_function)
}

/// Generate a random name for the wrapper function
fn generate_wrapper_name() -> String {
    let mut rng = rand::rng();
    let length = rng.random_range(15..25);
    let mut name = String::with_capacity(length + 2);
    let dic = [
        "0", "O", "o", "Ο", "ο", "θ", "Θ", "О", "о", "〇", "०", "০", "۰", "°", "○", // 0
        "1", "l", "I", "|", "Ⅰ", "і", "Ӏ", "１", "ǀ", "Ι", "І", // 1
        "2", "２", "3", "３", "4", "４", "5", "５", "Ƽ", "6", "６", "7", "７", "8", "８", "9", "９", "a", "а", "α",
        "а", "ɑ", "ａ", "ά", "à", "á", "â", "ã", "ä", "å", "ā", "ă", "ą", "ǎ", "ǻ", "ḁ", "ạ", "ả", "ấ", "ầ", "ẩ", "ẫ",
        "ậ", "ắ", "ằ", "ẳ", "ẵ", "ặ", "i", "і", "ì", "í", "î", "ï", "ĩ", "ī", "ĭ", "į", "ı", "ǐ", "ỉ", "ị", "c", "с",
        "ć", "ĉ", "ċ", "č", "ç", "ḉ", "s", "ѕ", "ś", "ŝ", "ş", "š", "ṡ", "ṣ", "ṥ", "ṧ", "ṩ", "u", "μ", "ù", "ú", "û",
        "ü", "ũ", "ū", "ŭ", "ů", "ű", "ų", "ǔ", "ǖ", "ǘ", "ǚ", "ǜ", "ụ", "ủ", "ứ", "ừ", "ử", "ữ", "ự", "υ", "r", "г",
        "ŕ", "ŗ", "ř", "ṙ", "ṛ", "ṝ", "ṟ",
    ];

    for _ in 0..length {
        let chars = dic[rng.random_range(0..dic.len())];
        name.push_str(chars);
    }

    name.push_str("$$");
    name
}

/// Copy function attributes from source to target
fn copy_function_attributes<'a>(target: &FunctionValue<'a>, source: &FunctionValue<'a>) {
    let fun_attributes = source.attributes(AttributeLoc::Function);
    let mut params_attributes = Vec::new();
    for i in 0..source.count_params() {
        params_attributes.push(source.attributes(AttributeLoc::Param(i)));
    }
    let return_attributes = source.attributes(AttributeLoc::Return);

    for x in fun_attributes {
        target.add_attribute(AttributeLoc::Function, x);
    }

    for x in return_attributes {
        target.add_attribute(AttributeLoc::Return, x);
    }

    for (index, attr) in params_attributes.iter().enumerate() {
        for x in attr {
            target.add_attribute(AttributeLoc::Param(index as u32), *x);
        }
    }
}

/// Create the body of the wrapper function
fn create_wrapper_body<'a>(
    ctx: ContextRef<'a>,
    wrapper_function: &FunctionValue<'a>,
    called_function: &FunctionValue<'a>,
) -> anyhow::Result<()> {
    let entry_bb = ctx.append_basic_block(*wrapper_function, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(entry_bb);

    // Collect wrapper function parameters
    let args: Vec<BasicMetadataValueEnum> = wrapper_function.get_param_iter().map(|param| param.into()).collect();

    // Create call to the original function
    let call_result = builder.build_call(*called_function, &args, "wrapped_call")?;

    // Handle return based on function return type
    let return_type = wrapper_function.get_type().get_return_type();
    if let Some(ret_type) = return_type {
        match ret_type {
            BasicTypeEnum::IntType(_)
            | BasicTypeEnum::FloatType(_)
            | BasicTypeEnum::PointerType(_)
            | BasicTypeEnum::ArrayType(_)
            | BasicTypeEnum::StructType(_)
            | BasicTypeEnum::VectorType(_)
            | BasicTypeEnum::ScalableVectorType(_) => match call_result.try_as_basic_value() {
                Left(basic_value) => {
                    builder.build_return(Some(&basic_value))?;
                },
                Right(_) => {
                    builder.build_return(None)?;
                },
            },
        }
    } else {
        builder.build_return(None)?;
    }

    Ok(())
}

/// Replace the original call instruction with a call to the wrapper function
fn replace_call_with_wrapper<'a>(
    call_inst: &InstructionValue<'a>,
    wrapper_function: &FunctionValue<'a>,
) -> anyhow::Result<Option<InstructionValue<'a>>> {
    let ctx = wrapper_function.get_type().get_context();
    let builder = ctx.create_builder();

    // Position builder before the original call
    builder.position_before(call_inst);

    // Collect arguments from the original call instruction
    let num_operands = call_inst.get_num_operands();
    let mut args: Vec<BasicMetadataValueEnum> = Vec::new();

    // Skip the last operand which is the function being called
    for i in 0..num_operands.saturating_sub(1) {
        if let Some(arg) = call_inst.get_operand(i).unwrap().left() {
            args.push(arg.into());
        }
    }

    // Create new call to wrapper function
    let new_call = builder.build_call(*wrapper_function, &args, "wrapper_call")?;

    // Replace all uses of the old instruction with the new call
    if !call_inst.get_type().is_void_type() {
        let new_call_inst = (new_call.as_value_ref() as LLVMValueRef).into_instruction_value();
        call_inst.replace_all_uses_with(&new_call_inst);
    }

    // Remove the old instruction
    call_inst.erase_from_basic_block();

    // Return the new call instruction
    let new_call_inst = (new_call.as_value_ref() as LLVMValueRef).into_instruction_value();
    Ok(Some(new_call_inst))
}

use crate::config::Config;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::append_to_compiler_used;
use amice_macro::amice;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::{AnyTypeEnum, BasicTypeEnum};
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, InstructionOpcode,
    InstructionValue, PointerValue,
};
use llvm_plugin::inkwell::{builder::Builder, context::Context, Either::Left, Either::Right};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};
use rand::Rng;

#[amice(priority = 970, name = "FunctionWrapper", position = PassPosition::PipelineStart)]
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

        debug!(
            "(function-wrapper) initialized with enable={}, probability={}%, times={}",
            self.enable, self.probability, self.times
        );

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

            // Check if function should be obfuscated (using similar logic from other passes)
            let function_name = function.get_name().to_str().unwrap_or("");
            if should_skip_function(function_name) {
                continue;
            }

            debug!("(function-wrapper) processing function: {}", function_name);

            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if matches!(inst.get_opcode(), InstructionOpcode::Call | InstructionOpcode::Invoke) {
                        // Apply probability check
                        if rand::thread_rng().gen_range(0..100) < self.probability {
                            if let Some(called_func) = get_called_function(&inst) {
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
                    }
                    Ok(None) => {
                        debug!("(function-wrapper) call instruction was not wrapped (filtered out)");
                    }
                    Err(e) => {
                        error!("(function-wrapper) failed to handle call instruction: {}", e);
                    }
                }
            }
            current_call_instructions = next_call_instructions;
        }

        debug!("(function-wrapper) completed function wrapper pass");
        PreservedAnalyses::None
    }
}

/// Check if a function should be skipped from obfuscation
fn should_skip_function(name: &str) -> bool {
    // Skip intrinsics, compiler-generated functions, and system functions
    name.starts_with("llvm.")
        || name.starts_with("clang.")
        || name.starts_with("__")
        || name.starts_with("@")
        || name.is_empty()
}

/// Extract called function from a call instruction
fn get_called_function<'a>(inst: &InstructionValue<'a>) -> Option<FunctionValue<'a>> {
    match inst.get_opcode() {
        InstructionOpcode::Call | InstructionOpcode::Invoke => {
            let operand_num = inst.get_num_operands();
            if operand_num == 0 {
                return None;
            }

            let callee = inst.get_operand(operand_num - 1).unwrap().left()?;
            let callee_ptr = callee.into_pointer_value();
            unsafe { FunctionValue::new(callee_ptr.as_value_ref()) }
        }
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

    let function_name = called_function.get_name().to_str().unwrap_or("");
    if should_skip_function(function_name) {
        debug!("(function-wrapper) skipping function: {}", function_name);
        return Ok(None);
    }

    debug!("(function-wrapper) wrapping call to function: {}", function_name);

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

    // Build parameter types from call instruction arguments
    let mut param_types: Vec<BasicTypeEnum> = Vec::new();
    let num_operands = call_inst.get_num_operands();
    
    // Skip the last operand which is the function being called
    for i in 0..num_operands.saturating_sub(1) {
        if let Some(arg) = call_inst.get_operand(i).unwrap().left() {
            param_types.push(arg.get_type());
        }
    }

    // Get return type from the call instruction
    let return_type = call_inst.get_type();
    
    // Create function type - collect BasicMetadataTypeEnum for function signature
    let param_meta_types: Vec<_> = param_types.iter().map(|t| (*t).into()).collect();
    let wrapper_fn_type = if return_type.is_void_type() {
        ctx.void_type().fn_type(&param_meta_types, false)
    } else {
        // Convert AnyTypeEnum to appropriate function type
        match return_type {
            AnyTypeEnum::IntType(int_type) => {
                int_type.fn_type(&param_meta_types, false)
            }
            AnyTypeEnum::FloatType(float_type) => {
                float_type.fn_type(&param_meta_types, false)
            }
            AnyTypeEnum::PointerType(ptr_type) => {
                ptr_type.fn_type(&param_meta_types, false)
            }
            AnyTypeEnum::ArrayType(arr_type) => {
                arr_type.fn_type(&param_meta_types, false)
            }
            AnyTypeEnum::StructType(struct_type) => {
                struct_type.fn_type(&param_meta_types, false)
            }
            AnyTypeEnum::VectorType(vec_type) => {
                vec_type.fn_type(&param_meta_types, false)
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported return type for function wrapping"));
            }
        }
    };

    // Generate random wrapper function name
    let wrapper_name = generate_wrapper_name();
    
    // Create the wrapper function
    let wrapper_function = module.add_function(&wrapper_name, wrapper_fn_type, Some(Linkage::Internal));

    // Copy some attributes from the original function (simplified)
    copy_function_attributes(&wrapper_function, &called_function);

    // Mark as used to prevent elimination
    append_to_compiler_used(module, wrapper_function.as_global_value());

    // Create the wrapper function body
    create_wrapper_body(&ctx, &wrapper_function, &called_function)?;

    // Replace the call instruction to use the wrapper function
    replace_call_with_wrapper(call_inst, &wrapper_function)
}

/// Generate a random name for the wrapper function
fn generate_wrapper_name() -> String {
    let mut rng = rand::thread_rng();
    let length = rng.gen_range(15..25);
    let mut name = String::with_capacity(length + 8);
    name.push_str("Hack");
    
    for _ in 0..length {
        let chars = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let idx = rng.gen_range(0..chars.len());
        name.push(chars[idx] as char);
    }
    
    name.push_str("END");
    name
}

/// Copy function attributes from source to target
fn copy_function_attributes<'a>(target: &FunctionValue<'a>, _source: &FunctionValue<'a>) {
    // Add noinline attribute to prevent optimization from undoing our work
    let ctx = target.get_type().get_context();
    let noinline_kind = Attribute::get_named_enum_kind_id("noinline");
    let noinline_attr = ctx.create_enum_attribute(noinline_kind, 0);
    target.add_attribute(AttributeLoc::Function, noinline_attr);
    
    // TODO: Copy more comprehensive attributes if needed
    // This is a simplified version - full attribute copying would require
    // more complex LLVM attribute manipulation
}

/// Create the body of the wrapper function
fn create_wrapper_body<'a>(
    ctx: &Context,
    wrapper_function: &FunctionValue<'a>,
    called_function: &FunctionValue<'a>,
) -> anyhow::Result<()> {
    let entry_bb = ctx.append_basic_block(*wrapper_function, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(entry_bb);

    // Collect wrapper function parameters
    let args: Vec<BasicMetadataValueEnum> = wrapper_function
        .get_param_iter()
        .map(|param| param.into())
        .collect();

    // Create call to the original function
    let call_result = builder.build_call(*called_function, &args, "wrapped_call")?;

    // Handle return based on function return type
    let return_type = wrapper_function.get_type().get_return_type();
    if let Some(ret_type) = return_type {
        match ret_type {
            BasicTypeEnum::IntType(_) |
            BasicTypeEnum::FloatType(_) |
            BasicTypeEnum::PointerType(_) |
            BasicTypeEnum::ArrayType(_) |
            BasicTypeEnum::StructType(_) |
            BasicTypeEnum::VectorType(_) => {
                match call_result.try_as_basic_value() {
                    Left(basic_value) => {
                        builder.build_return(Some(&basic_value))?;
                    }
                    Right(_) => {
                        builder.build_return(None)?;
                    }
                }
            }
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
        match new_call.try_as_basic_value() {
            Left(basic_value) => {
                call_inst.replace_all_uses_with(&basic_value.as_instruction_value());
            }
            Right(_) => {
                // Do nothing for non-basic values
            }
        }
    }

    // Remove the old instruction
    call_inst.erase_from_basic_block();

    // Return the new call instruction
    let new_inst = unsafe { InstructionValue::new(new_call.as_value_ref()) };
    Ok(Some(new_inst))
}
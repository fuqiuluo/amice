use crate::config::{Config, FunctionWrapperConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, InstructionExt, LLVMValueRefExt, ModuleExt};
use amice_macro::amice;
use amice_plugin::PreservedAnalyses;
use amice_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use amice_plugin::inkwell::context::ContextRef;
use amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::types::BasicTypeEnum;
use amice_plugin::inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, CallSiteValue, FunctionValue, InstructionOpcode, InstructionValue, ValueKind,
};
use rand::Rng;

#[amice(
    priority = 1010,
    name = "FunctionWrapper",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = FunctionWrapperConfig,
)]
#[derive(Default)]
pub struct FunctionWrapper {}

impl AmicePass for FunctionWrapper {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.function_wrapper.clone();
        self.default_config.probability = cfg.function_wrapper.probability.min(100);
        self.default_config.times = cfg.function_wrapper.times.max(1);
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        debug!("default_config.enable = {}", self.default_config.enable);

        // Collect call sites that need to be wrapped
        let mut call_instructions = Vec::new();
        let mut func_count = 0;
        let mut total_funcs = 0;
        for function in module.get_functions() {
            total_funcs += 1;
            if function.is_llvm_function() {
                //debug!("skip llvm function: {:?}", function.get_name());
                continue;
            }
            if function.is_inline_marked() {
                //debug!("skip inline function: {:?}", function.get_name());
                continue;
            }
            if function.is_undef_function() {
                //debug!("skip undef function: {:?}", function.get_name());
                continue;
            }
            func_count += 1;

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                //debug!("function {:?} skipped: enable=false", function.get_name());
                continue;
            }

            if function_has_exception_handling(function) {
                debug!(
                    "function {:?} has exception handling instructions, skipping",
                    function.get_name()
                );
                continue;
            }

            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if inst.get_opcode() == InstructionOpcode::Call {
                        // Apply probability check
                        if rand::random_range(0..100) < cfg.probability {
                            if let Some(called_func) = get_called_function(&inst) {
                                //debug!("adding call to function: {:?}", called_func.get_name());
                                call_instructions.push((inst, called_func));
                            }
                        }
                    }
                }
            }
        }

        if call_instructions.is_empty() {
            debug!(
                "no call instructions found, total_funcs={}, valid_funcs={}",
                total_funcs, func_count
            );
            return Ok(PreservedAnalyses::All);
        }

        debug!("starting function wrapper pass");
        debug!("collected {} call sites for wrapping", call_instructions.len());

        // Apply wrapper transformation multiple times
        let mut current_call_instructions = call_instructions;
        for iteration in 0..self.default_config.times {
            debug!(
                "applying wrapper iteration {}/{}",
                iteration + 1,
                self.default_config.times
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
                        debug!("call instruction was not wrapped (filtered out)");
                    },
                    Err(e) => {
                        error!("failed to handle call instruction: {}", e);
                    },
                }
            }
            current_call_instructions = next_call_instructions;
        }

        debug!("completed function wrapper pass");

        Ok(PreservedAnalyses::None)
    }
}

/// Extract called function from a call instruction
fn get_called_function<'a>(inst: &InstructionValue<'a>) -> Option<FunctionValue<'a>> {
    match inst.get_opcode() {
        InstructionOpcode::Call => inst.into_call_inst().get_call_function(),
        _ => None,
    }
}

fn function_has_exception_handling(function: FunctionValue<'_>) -> bool {
    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            if matches!(
                inst.get_opcode(),
                InstructionOpcode::Invoke
                    | InstructionOpcode::LandingPad
                    | InstructionOpcode::Resume
                    | InstructionOpcode::CatchSwitch
                    | InstructionOpcode::CatchPad
                    | InstructionOpcode::CatchRet
                    | InstructionOpcode::CleanupPad
                    | InstructionOpcode::CleanupRet
                    | InstructionOpcode::CallBr
            ) {
                return true;
            }
        }
    }

    false
}

/// Handle a single call instruction by creating a wrapper function
fn handle_call_instruction<'a>(
    module: &mut Module<'a>,
    call_inst: InstructionValue<'a>,
    called_function: Option<FunctionValue<'a>>,
) -> anyhow::Result<Option<InstructionValue<'a>>> {
    let Some(called_function) = called_function else {
        //debug!("skipping call with no function");
        return Ok(None);
    };

    // Skip intrinsic functions
    if called_function.get_intrinsic_id() != 0 {
        //debug!("skipping intrinsic function");
        return Ok(None);
    }

    if called_function.is_llvm_function() || called_function.is_inline_marked() {
        return Ok(None);
    }

    if called_function.get_type().is_var_arg() {
        //debug!("skipping varargs function: {:?}", called_function.get_name());
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

    debug!(
        "(FunctionWrapper) creating wrapper '{}' for function '{:?}'",
        wrapper_name,
        called_function.get_name()
    );

    // Create the wrapper function with the same signature as the called function
    let wrapper_function = module.add_function(&wrapper_name, called_fn_type, Some(Linkage::Internal));
    let original_call_site = (*call_inst).into_call_inst().into_call_site_value();

    copy_function_attributes(&wrapper_function, &called_function);
    copy_call_site_attributes_to_function(&wrapper_function, original_call_site);
    wrapper_function.set_call_conventions(original_call_site.get_call_convention());

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
    target.set_call_conventions(source.get_call_conventions());
    copy_function_attributes_at(target, *source, AttributeLoc::Function);
    copy_function_attributes_at(target, *source, AttributeLoc::Return);

    for i in 0..source.count_params() {
        copy_function_attributes_at(target, *source, AttributeLoc::Param(i));
    }
}

fn copy_function_attributes_at<'a>(target: &FunctionValue<'a>, source: FunctionValue<'a>, loc: AttributeLoc) {
    for attr in source.attributes(loc) {
        add_function_attribute_if_missing(target, loc, attr);
    }
}

fn copy_call_site_attributes_to_function<'a>(target: &FunctionValue<'a>, source: CallSiteValue<'a>) {
    copy_call_site_attributes_to_function_at(target, source, AttributeLoc::Return);

    for i in 0..source.count_arguments() {
        copy_call_site_attributes_to_function_at(target, source, AttributeLoc::Param(i));
    }
}

fn copy_call_site_attributes_to_function_at<'a>(
    target: &FunctionValue<'a>,
    source: CallSiteValue<'a>,
    loc: AttributeLoc,
) {
    for attr in source.attributes(loc) {
        add_function_attribute_if_missing(target, loc, attr);
    }
}

fn add_function_attribute_if_missing<'a>(function: &FunctionValue<'a>, loc: AttributeLoc, attr: Attribute) {
    if !function.attributes(loc).contains(&attr) {
        function.add_attribute(loc, attr);
    }
}

fn copy_call_site_attributes<'a>(target: CallSiteValue<'a>, source: CallSiteValue<'a>) {
    target.set_call_convention(source.get_call_convention());

    #[cfg(any(
        feature = "llvm21-1",
        feature = "llvm22-1",
        feature = "llvm20-1",
        feature = "llvm19-1",
        feature = "llvm18-1"
    ))]
    {
        target.set_tail_call(source.is_tail_call());
        target.set_tail_call_kind(source.get_tail_call_kind());
    }

    copy_call_site_attributes_at(target, source, AttributeLoc::Function);
    copy_call_site_attributes_at(target, source, AttributeLoc::Return);

    for i in 0..source.count_arguments() {
        copy_call_site_attributes_at(target, source, AttributeLoc::Param(i));
    }
}

fn copy_call_site_attributes_at<'a>(target: CallSiteValue<'a>, source: CallSiteValue<'a>, loc: AttributeLoc) {
    for attr in source.attributes(loc) {
        add_call_site_attribute_if_missing(target, loc, attr);
    }
}

fn add_call_site_attribute_if_missing<'a>(call_site: CallSiteValue<'a>, loc: AttributeLoc, attr: Attribute) {
    if !call_site.attributes(loc).contains(&attr) {
        call_site.add_attribute(loc, attr);
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
    call_result.set_call_convention(called_function.get_call_conventions());
    copy_function_attributes_to_call_site(call_result, called_function);
    copy_function_attributes_to_call_site(call_result, wrapper_function);

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
                ValueKind::Basic(basic_value) => {
                    builder.build_return(Some(&basic_value))?;
                },
                ValueKind::Instruction(_) => {
                    builder.build_return(None)?;
                },
            },
        }
    } else {
        builder.build_return(None)?;
    }

    Ok(())
}

fn copy_function_attributes_to_call_site<'a>(target: CallSiteValue<'a>, source: &FunctionValue<'a>) {
    copy_function_attributes_to_call_site_at(target, *source, AttributeLoc::Return);

    for i in 0..source.count_params() {
        copy_function_attributes_to_call_site_at(target, *source, AttributeLoc::Param(i));
    }
}

fn copy_function_attributes_to_call_site_at<'a>(
    target: CallSiteValue<'a>,
    source: FunctionValue<'a>,
    loc: AttributeLoc,
) {
    for attr in source.attributes(loc) {
        add_call_site_attribute_if_missing(target, loc, attr);
    }
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
        if let Some(arg) = call_inst.get_operand(i).unwrap().value() {
            args.push(arg.into());
        }
    }

    // Create new call to wrapper function
    let old_call = (*call_inst).into_call_inst().into_call_site_value();
    let new_call = builder.build_call(*wrapper_function, &args, "wrapper_call")?;
    copy_call_site_attributes(new_call, old_call);

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

#[cfg(test)]
mod tests {
    use super::*;
    use amice_plugin::inkwell::AddressSpace;
    use amice_plugin::inkwell::context::Context;
    use amice_plugin::inkwell::types::AnyType;
    use amice_plugin::inkwell::values::BasicValue;

    fn has_attribute(call_site: CallSiteValue<'_>, loc: AttributeLoc, attr: Attribute) -> bool {
        call_site.attributes(loc).contains(&attr)
    }

    fn function_has_attribute(function: FunctionValue<'_>, loc: AttributeLoc, attr: Attribute) -> bool {
        function.attributes(loc).contains(&attr)
    }

    #[test]
    fn preserves_sret_attributes_for_wrapper_calls() {
        let context = Context::create();
        let mut module = context.create_module("issue_83_sret");
        let builder = context.create_builder();

        let i64_type = context.i64_type();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let result_type = context.struct_type(&[i64_type.into()], false);
        let sret_attr = context.create_type_attribute(
            Attribute::get_named_enum_kind_id("sret"),
            result_type.as_any_type_enum(),
        );

        let callee_type = context.void_type().fn_type(
            &[ptr_type.into(), ptr_type.into(), i64_type.into(), i64_type.into()],
            false,
        );
        let callee = module.add_function("substr_like", callee_type, None);
        callee.add_attribute(AttributeLoc::Param(0), sret_attr);

        let caller_type = context.void_type().fn_type(&[], false);
        let caller = module.add_function("caller", caller_type, None);
        let entry = context.append_basic_block(caller, "entry");
        builder.position_at_end(entry);

        let result_slot = builder.build_alloca(result_type, "result").unwrap();
        let self_slot = builder.build_alloca(result_type, "self").unwrap();
        let original_call = builder
            .build_call(
                callee,
                &[
                    result_slot.as_basic_value_enum().into(),
                    self_slot.as_basic_value_enum().into(),
                    i64_type.const_int(0, false).into(),
                    i64_type.const_int(u64::MAX, false).into(),
                ],
                "substr_call",
            )
            .unwrap();
        original_call.add_attribute(AttributeLoc::Param(0), sret_attr);
        builder.build_return(None).unwrap();

        let original_inst = (original_call.as_value_ref() as LLVMValueRef).into_instruction_value();
        let new_inst = create_wrapper_function(&mut module, &original_inst, callee)
            .unwrap()
            .expect("call should be wrapped");
        let new_call = new_inst.into_call_inst().into_call_site_value();
        let wrapper = get_called_function(&new_inst).expect("wrapper call should be direct");
        let inner_call = wrapper
            .get_first_basic_block()
            .expect("wrapper should have an entry block")
            .get_instructions()
            .find(|inst| inst.get_opcode() == InstructionOpcode::Call)
            .expect("wrapper should call the original function")
            .into_call_inst()
            .into_call_site_value();

        assert!(function_has_attribute(wrapper, AttributeLoc::Param(0), sret_attr));
        assert!(has_attribute(new_call, AttributeLoc::Param(0), sret_attr));
        assert!(has_attribute(inner_call, AttributeLoc::Param(0), sret_attr));
        module.verify().expect("transformed module should verify");
    }
}

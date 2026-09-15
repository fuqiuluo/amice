use crate::config::{Config, ParamAggregateConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BuilderExt, CallInst, FunctionExt, InstructionExt, LLVMValueRefExt, VerifyResult};
use amice_macro::amice;
use amice_plugin::PreservedAnalyses;
use amice_plugin::inkwell::AddressSpace;
use amice_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use amice_plugin::inkwell::llvm_sys::core::LLVMBuildFreeze;
use amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use amice_plugin::inkwell::module::{Linkage, Module};
use amice_plugin::inkwell::types::{BasicType, StructType};
use amice_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode};
use anyhow::anyhow;
use log::log_enabled;
use rand::prelude::SliceRandom;
use std::collections::HashMap;

const MAX_STRUCT_SIZE: usize = 4096;

#[amice(
    priority = 1120,
    name = "ParamAggregate",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = ParamAggregateConfig,
)]
#[derive(Default)]
pub struct ParamAggregate {}

impl AmicePass for ParamAggregate {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.param_aggregate.clone();
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

        if functions.is_empty() {
            return Ok(PreservedAnalyses::All);
        }

        let mut call_instructions = Vec::new();
        for function in functions {
            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if inst.get_opcode() == InstructionOpcode::Call {
                        call_instructions.push(inst.into_call_inst());
                    }
                }
            }
        }

        let mut param_aggregated_functions = HashMap::<FunctionValue, ParamAggregatedFunction>::new();
        for call_inst in call_instructions.iter() {
            let Some(call_function) = call_inst.get_call_function() else {
                continue;
            };

            if param_aggregated_functions.contains_key(&call_function) {
                continue;
            }

            match create_param_aggregated_function(module, call_function) {
                Err(e) => {
                    if log_enabled!(log::Level::Debug) {
                        warn!("failed handle_function {}", e);
                    }
                },
                Ok(new_function) => {
                    if let VerifyResult::Broken(msg) = new_function.function.verify_function() {
                        error!("failed verify_function {}", msg);
                    }

                    param_aggregated_functions.insert(call_function, new_function);
                },
            }
        }

        if let Err(e) = replace_function_call(module, call_instructions, param_aggregated_functions) {
            error!("failed replace_function_call {}", e);
        }

        Ok(PreservedAnalyses::None)
    }
}

fn replace_function_call<'a>(
    module: &mut Module<'a>,
    call_instructions: Vec<CallInst<'a>>,
    param_aggregated_functions: HashMap<FunctionValue<'a>, ParamAggregatedFunction<'a>>,
) -> anyhow::Result<()> {
    let ctx = module.get_context();

    let i64_type = ctx.i64_type();

    let builder = ctx.create_builder();

    for call_inst in call_instructions {
        let Some(call_function) = call_inst.get_call_function() else {
            continue;
        };

        let Some(param_aggregated_function) = param_aggregated_functions.get(&call_function) else {
            continue;
        };

        let original_call_site = call_inst.into_call_site_value();
        if has_no_memory_effects(original_call_site.attributes(AttributeLoc::Function)) {
            continue;
        }

        builder.position_before(&call_inst);
        let struct_type = param_aggregated_function.struct_type;
        let st_ptr = builder.build_alloca(struct_type, "")?;

        let mut args = Vec::new();
        let num_operands = call_inst.get_num_operands();
        for i in 0..num_operands.saturating_sub(1) {
            if let Some(arg) = call_inst.get_operand(i).unwrap().value() {
                args.push(arg);
            }
        }

        for (arg_index, field_index) in &param_aggregated_function.args_index {
            if *arg_index == usize::MAX {
                if rand::random_range(0..100) < 40 {
                    let st_gep = builder.build_struct_gep2(struct_type, st_ptr, *field_index as u32, "dummy")?;

                    builder.build_store(st_gep, i64_type.const_int(rand::random(), false))?;
                }
                continue;
            }

            let st_gep =
                builder.build_struct_gep2(struct_type, st_ptr, *field_index as u32, &format!("arg{}", arg_index))?;
            let arg = args[*arg_index];
            // An ABI argument may be poison even when it is not a literal
            // poison constant. Freeze each value before materializing it in a
            // shared aggregate so SROA cannot spread poison into live fields.
            let frozen_arg = unsafe {
                LLVMBuildFreeze(
                    builder.as_mut_ptr(),
                    arg.as_value_ref(),
                    b"arg.freeze\0".as_ptr().cast(),
                )
                .into_basic_value_enum()
            };
            builder.build_store(st_gep, frozen_arg)?;
        }

        let new_call = builder.build_call(param_aggregated_function.function, &[st_ptr.into()], "")?;
        new_call.set_call_convention(param_aggregated_function.function.get_call_conventions());
        // Calls with memory(none)/readnone were excluded above. The remaining
        // properties remain valid through this transparent wrapper.
        for attr in original_call_site.attributes(AttributeLoc::Function) {
            new_call.add_attribute(AttributeLoc::Function, attr);
        }
        for attr in original_call_site.attributes(AttributeLoc::Return) {
            new_call.add_attribute(AttributeLoc::Return, attr);
        }

        if !call_inst.get_type().is_void_type() {
            let new_call_inst = (new_call.as_value_ref() as LLVMValueRef).into_instruction_value();
            call_inst.replace_all_uses_with(&new_call_inst);
        }

        call_inst.erase_from_basic_block();
    }

    Ok(())
}

fn has_no_memory_effects(attrs: Vec<Attribute>) -> bool {
    let memory_kind = Attribute::get_named_enum_kind_id("memory");
    let readnone_kind = Attribute::get_named_enum_kind_id("readnone");
    attrs.into_iter().any(|attr| {
        !attr.is_string()
            && ((memory_kind != 0 && attr.get_enum_kind_id() == memory_kind && attr.get_enum_value() == 0)
                || (readnone_kind != 0 && attr.get_enum_kind_id() == readnone_kind))
    })
}

struct ParamAggregatedFunction<'a> {
    function: FunctionValue<'a>,
    struct_type: StructType<'a>,
    args_index: HashMap<usize, usize>,
}

impl<'a> ParamAggregatedFunction<'a> {
    fn new(function: FunctionValue<'a>, struct_type: StructType<'a>) -> Self {
        Self {
            function,
            struct_type,
            args_index: HashMap::new(),
        }
    }

    fn add_arg_index(&mut self, index: usize, arg_index: usize) {
        self.args_index.insert(index, arg_index);
    }
}

fn create_param_aggregated_function<'a>(
    module: &mut Module<'a>,
    function: FunctionValue<'a>,
) -> anyhow::Result<ParamAggregatedFunction<'a>> {
    if function.is_inline_marked() || function.is_llvm_function() || function.is_undef_function() {
        return Err(anyhow!(
            "function is inline marked or llvm function or undef function: {:?}",
            function.get_name()
        ));
    }

    if function.count_params() <= 1 {
        return Err(anyhow!(
            "function param count is less than 2: {:?}",
            function.get_name()
        ));
    }

    if function.get_type().is_var_arg() {
        return Err(anyhow!("function is var arg: {:?}", function.get_name()));
    }

    // A pointer-based aggregate wrapper necessarily reads its argument
    // record. Wrapping a memory(none)/readnone function would make the old
    // call-site memory contract false, so leave such functions unchanged.
    if has_no_memory_effects(function.attributes(AttributeLoc::Function)) {
        return Err(anyhow!(
            "function has no memory effects and cannot use a pointer aggregate: {:?}",
            function.get_name()
        ));
    }

    // These attributes impose call-site constraints that cannot safely be
    // forwarded through a plain aggregate wrapper (for example, preallocated
    // requires a matching operand bundle). Compare raw LLVM IDs so this stays
    // compatible when newer LLVM releases add unrelated attribute kinds.
    let unsupported_param_attr_ids = ["inalloca", "preallocated", "swifterror"]
        .map(Attribute::get_named_enum_kind_id)
        .into_iter()
        .filter(|id| *id != 0)
        .collect::<Vec<_>>();
    for i in 0..function.count_params() {
        let param_attrs = function.attributes(AttributeLoc::Param(i));
        for attr in param_attrs {
            if attr.is_string() {
                continue;
            }
            if unsupported_param_attr_ids.contains(&attr.get_enum_kind_id()) {
                return Err(anyhow!(
                    "function has an unsupported ABI parameter attribute: {:?}",
                    function.get_name()
                ));
            }
        }
    }

    let ctx = module.get_context();

    let i64_type = ctx.i64_type();

    let builder = ctx.create_builder();

    let mut struct_types = Vec::new();
    let mut struct_size = 0;

    for (index, param) in function.get_params().iter().enumerate() {
        let typ = param.get_type();
        // if !typ.is_sized() {
        //     return Err(anyhow!("param type is not sized"));
        // }
        if let Some(size_int) = typ.size_of()
            && let Some(size) = size_int.get_zero_extended_constant()
        {
            struct_size += size;
        }
        struct_types.push((index, typ));
    }

    if struct_size > MAX_STRUCT_SIZE as u64 {
        return Err(anyhow!(
            "struct size is too large: {} bytes: {:?}",
            struct_size,
            function.get_name()
        ));
    }

    for _ in 0..rand::random_range(0..(struct_types.len() * 2 + 1)) {
        struct_types.push((usize::MAX, i64_type.as_basic_type_enum()));
    }

    struct_types.shuffle(&mut rand::rng());

    if log_enabled!(log::Level::Debug) {
        debug!("(param-aggregate) handle function: {:?}", function.get_name());
        debug!("(param-aggregate) struct_types: {:?}", struct_types);
    }

    let field_types = struct_types.iter().map(|(_, typ)| *typ).collect::<Vec<_>>();
    let struct_type = ctx.struct_type(&field_types, false);

    let function_type = function.get_type();
    let aggregated_function_type = function_type
        .get_return_type()
        .ok_or(anyhow!("failed get return type: {:?}", function.get_name()))?
        .fn_type(&[struct_type.ptr_type(AddressSpace::default()).into()], false);

    // Keep the aggregate ABI behind a noinline wrapper. Calling the original
    // function avoids the C++ CloneFunctionInto API, whose symbol is not
    // exported by every Android NDK libLLVM build (notably older NDKs).
    let aggregated_function = module.add_function(
        &format!("{}.param.aggregate", function.get_name().to_str().unwrap_or("unknown")),
        aggregated_function_type,
        None,
    );
    aggregated_function.set_linkage(Linkage::Internal);
    aggregated_function.set_call_conventions(function.get_call_conventions());
    for attr in function.attributes(AttributeLoc::Return) {
        aggregated_function.add_attribute(AttributeLoc::Return, attr);
    }
    let noinline_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("noinline"), 0);
    aggregated_function.add_attribute(AttributeLoc::Function, noinline_attr);

    let struct_ptr = aggregated_function.get_first_param().unwrap().into_pointer_value();

    let entry_block = ctx.append_basic_block(aggregated_function, "entry");

    builder.position_at_end(entry_block);
    let mut args = Vec::new();
    for (struct_field_index, (arg_index, arg_type)) in struct_types.iter().enumerate() {
        if *arg_index == usize::MAX {
            continue;
        }
        let field_ptr = builder.build_struct_gep2(
            struct_type,
            struct_ptr,
            struct_field_index as u32,
            &format!("arg{}", arg_index),
        )?;
        let field_val = builder.build_load2(*arg_type, field_ptr, &format!("load{}", arg_index))?;
        args.push((arg_index, field_val));
    }
    args.sort_by_key(|x| x.0);
    let args = args.into_iter().map(|x| x.1.into()).collect::<Vec<_>>();
    let ret = builder.build_call(function, &args, "call_ret")?;
    ret.set_call_convention(function.get_call_conventions());
    // Keep the semantic boundary of the wrapper explicit. This prevents a
    // chain of wrappers from being folded back into the original multi-arg
    // calling pattern, while unrelated calls remain optimizable.
    ret.add_attribute(AttributeLoc::Function, noinline_attr);
    for i in 0..function.count_params() {
        for attr in function.attributes(AttributeLoc::Param(i)) {
            ret.add_attribute(AttributeLoc::Param(i), attr);
        }
    }
    for attr in function.attributes(AttributeLoc::Return) {
        ret.add_attribute(AttributeLoc::Return, attr);
    }
    if let Some(_) = function_type.get_return_type() {
        let ret = ret
            .try_as_basic_value()
            .basic()
            .ok_or(anyhow!("failed cast to basic value"))?;
        builder.build_return(Some(&ret))?;
    } else {
        builder.build_return(None)?;
    }

    let mut param_aggregated_function = ParamAggregatedFunction::new(aggregated_function, struct_type);
    for (index, (arg_index, _)) in struct_types.iter().enumerate() {
        param_aggregated_function.add_arg_index(*arg_index, index);
    }

    Ok(param_aggregated_function)
}

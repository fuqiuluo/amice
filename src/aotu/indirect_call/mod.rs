use crate::config::{Config, IndirectCallConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BuilderExt, CallInst, FunctionExt, InstructionExt, LLVMValueRefExt, ModuleExt};
use amice_llvm::ptr_type;
use amice_macro::amice;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::AttributeLoc;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicValue, CallSiteValue, FunctionValue, GlobalValue, InstructionOpcode, InstructionValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(
    priority = 990,
    name = "IndirectCall",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = IndirectCallConfig,
)]
#[derive(Default)]
pub struct IndirectCall {
    enable: bool,
    xor_key: u32,
}

impl AmicePass for IndirectCall {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.indirect_call.clone();

        self.xor_key = cfg
            .indirect_call
            .xor_key
            .unwrap_or(if self.enable { rand::random::<u32>() } else { 0 });

        if self.xor_key != 0 {
            warn!(
                "Indirect call XOR key is set to {}, this may cause issues if the key is not known at runtime.",
                self.xor_key
            );
        }
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_undef_function() || function.is_llvm_function() {
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

        let mut likely_functions = Vec::new();
        let mut call_instructions = Vec::new();
        for function in &functions {
            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if inst.get_opcode() == InstructionOpcode::Call {
                        let operand_num = inst.get_num_operands();
                        if operand_num == 0 {
                            warn!("indirect call instruction with no operands found: {inst:?}");
                            continue;
                        }

                        let callee = inst.get_operand(operand_num - 1).unwrap().value();
                        let Some(callee) = callee else {
                            warn!("indirect call instruction with no callee found: {inst:?}");
                            continue;
                        };
                        let callee = callee.into_pointer_value();
                        let Some(callee) = (unsafe { FunctionValue::new(callee.as_value_ref()) }) else {
                            debug!("indirect call instruction with no function found: {inst:?}");
                            continue;
                        };

                        if callee.get_intrinsic_id() != 0 {
                            continue;
                        }

                        call_instructions.push((inst.into_call_inst(), callee));
                        if likely_functions.contains(&callee) {
                            continue;
                        }

                        likely_functions.push(callee);
                    }
                }
            }
        }

        let ctx = module.get_context();
        let i32_type = ctx.i32_type();
        let ptr_type = ptr_type!(ctx, i8_type);

        let likely_functions_values = likely_functions
            .iter()
            .map(|f| f.as_global_value())
            .map(|f| f.as_pointer_value())
            .collect::<Vec<_>>();

        let array_type = ptr_type.array_type(likely_functions.len() as u32);
        let initializer = ptr_type.const_array(&likely_functions_values);
        let global_fun_table = module.add_global(array_type, None, ".amice_indirect_call_table");
        global_fun_table.set_linkage(Linkage::Private);
        global_fun_table.set_initializer(&initializer);

        module.append_to_compiler_used(global_fun_table);

        let xor_key_global = if self.xor_key != 0 {
            let g = module.add_global(i32_type, None, ".amice_xor_key");
            g.set_linkage(Linkage::Private);
            g.set_initializer(&i32_type.const_int(self.xor_key as u64, false));
            g.set_constant(false);
            g.into()
        } else {
            None
        };

        #[cfg(any(
            feature = "llvm20-1",
            feature = "llvm19-1",
            feature = "llvm18-1",
            feature = "llvm17-0",
            feature = "llvm16-0",
            feature = "llvm15-0",
        ))]
        if let Err(e) = do_handle(
            self,
            module,
            &likely_functions,
            global_fun_table,
            &call_instructions,
            xor_key_global,
        ) {
            error!("failed to handle: {e}");
        }

        #[cfg(not(any(
            feature = "llvm20-1",
            feature = "llvm19-1",
            feature = "llvm18-1",
            feature = "llvm17-0",
            feature = "llvm16-0",
            feature = "llvm15-0",
        )))]
        error!("LLVM version is not supported");

        for f in functions {
            if f.verify_function_bool() {
                warn!("function {:?} is not verified", f.get_name());
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

// Only handle type 1 and 3
#[cfg(any(
    feature = "llvm20-1",
    feature = "llvm19-1",
    feature = "llvm18-1",
    feature = "llvm17-0",
    feature = "llvm16-0",
    feature = "llvm15-0",
))]
fn do_handle<'a>(
    pass: &IndirectCall,
    module: &mut Module<'_>,
    likely_functions: &Vec<FunctionValue>,
    global_fun_table: GlobalValue,
    call_instructions: &Vec<(CallInst, FunctionValue)>,
    xor_key_global: Option<GlobalValue>,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_type = ctx.i32_type();
    let pty_type = ptr_type!(ctx, i8_type);

    for (inst, function) in call_instructions {
        let index = likely_functions
            .iter()
            .position(|f| f.as_value_ref() == function.as_value_ref())
            .ok_or_else(|| anyhow::anyhow!("Function not found in likely functions"))?;
        let index_value = if pass.xor_key == 0 {
            i32_type.const_int(index as u64, false)
        } else {
            i32_type.const_int((index as u32 ^ pass.xor_key) as u64, false)
        };

        let builder = ctx.create_builder();
        builder.position_before(inst);
        let index_value = if xor_key_global.is_some() {
            let xor_key_value = builder.build_load2(i32_type, xor_key_global.unwrap().as_pointer_value(), "")?;
            builder.build_xor(index_value, xor_key_value.into_int_value(), "")?
        } else {
            index_value
        };
        let gep = builder.build_gep2(pty_type, global_fun_table.as_pointer_value(), &[index_value], "")?;
        let addr = builder.build_load2(pty_type, gep, "")?.into_pointer_value();

        let call_site = inst.into_call_site_value();
        let mut args = Vec::new();
        let fun_attributes = call_site.attributes(AttributeLoc::Function);
        let mut param_attributes = Vec::new();
        for i in 0..call_site.count_arguments() {
            let attr = call_site.attributes(AttributeLoc::Param(i));
            param_attributes.push(attr);

            let get_operand = inst
                .get_operand(i)
                .ok_or_else(|| anyhow::anyhow!("Indirect call instruction has no operand at index {i}"))?
                .value()
                .ok_or_else(|| anyhow::anyhow!("Indirect call instruction operand at index {i} is not a pointer"))?;
            args.push(get_operand);
        }
        let return_attributes = call_site.attributes(AttributeLoc::Return);

        let args = args.iter().map(|v| v.as_basic_value_enum().into()).collect::<Vec<_>>();

        let new_call_site = builder.build_indirect_call(function.get_type(), addr, &args, "")?;
        new_call_site.set_call_convention(call_site.get_call_convention());
        #[cfg(any(feature = "llvm20-1", feature = "llvm19-1", feature = "llvm18-1"))]
        {
            new_call_site.set_tail_call(call_site.is_tail_call());
            new_call_site.set_tail_call_kind(call_site.get_tail_call_kind());
        }
        for x in fun_attributes {
            new_call_site.add_attribute(AttributeLoc::Function, x);
        }
        for (i, x) in param_attributes.iter().enumerate() {
            for y in x {
                new_call_site.add_attribute(AttributeLoc::Param(i as u32), *y);
            }
        }
        for x in return_attributes {
            new_call_site.add_attribute(AttributeLoc::Return, x);
        }
        let new_call_inst = (new_call_site.as_value_ref() as LLVMValueRef).into_instruction_value();

        inst.replace_all_uses_with(&new_call_inst);
        inst.erase_from_basic_block();
    }
    Ok(())
}

// ==== type 1
// %7 = call i32 (ptr, ...) @printf(ptr noundef @.str, i32 noundef %5, i32 noundef %6)
// op[0] = .str
// op[1] = %5
// op[2] = %6
// op[3] = @printf
// ==== type 2
// %28 = call i32 %25(i32 noundef %26, i32 noundef %27)
// op[0] = %26
// op[1] = %27
// op[2] = %25
// ==== type 3
// call void @srand(i32 noundef %3) #3
// op[0] = %3
// op[1] = @srand

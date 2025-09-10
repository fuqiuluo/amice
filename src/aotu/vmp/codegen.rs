use crate::aotu::vmp::bytecode::VMPBytecodeEncoder;
use crate::aotu::vmp::compiler::VMPCompilerContext;
use crate::aotu::vmp::isa::VMPOpcode;
use crate::aotu::vmp::translator::BasicTypeEnumSized;
use amice_llvm::inkwell2::{BuilderExt, LLVMValueRefExt, ModuleExt};
use amice_llvm::ptr_type;
use anyhow::{Result, anyhow};
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::Linkage::LinkerPrivate;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::{ArrayType, BasicType, BasicTypeEnum, StringRadix, StructType};
use llvm_plugin::inkwell::values::{AsValueRef, BasicValueEnum, FunctionValue, GlobalValue};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use log::{Level, debug, log_enabled};
use std::future::poll_fn;
use std::path::PathBuf;

pub struct VMPCodeGenerator<'a, 'b> {
    module: &'b mut Module<'a>,
    encoder: VMPBytecodeEncoder,
}

struct RuntimeFunctions<'a> {
    avm_runtime_new: FunctionValue<'a>,
    avm_runtime_destroy: FunctionValue<'a>,
    avm_runtime_execute: FunctionValue<'a>,
}

impl<'a, 'b> VMPCodeGenerator<'a, 'b> {
    pub fn new(module: &'b mut Module<'a>) -> Result<Self> {
        Ok(Self {
            module,
            encoder: VMPBytecodeEncoder::new(),
        })
    }

    /// 将虚拟机指令序列编译成调用虚拟机运行时的LLVM IR
    pub fn compile_function_to_vm_call(&mut self, function: FunctionValue, context: VMPCompilerContext) -> Result<()> {
        // 序列化指令数据到全局常量
        let instructions_data = self.serialize_instructions_to_global(&context)?;

        let runtime_functions = self.declare_runtime_functions(context)?;

        // 清空原函数体，重新生成调用虚拟机的代码
        self.replace_function_body_with_vm_call(function, instructions_data, runtime_functions)?;

        Ok(())
    }

    /// 序列化指令数据到全局常量
    fn serialize_instructions_to_global(&mut self, compiler_context: &VMPCompilerContext) -> Result<GlobalValue<'a>> {
        let context = self.module.get_context();

        // 使用字节码编码器
        let bytecode_data = self
            .encoder
            .encode_instructions(compiler_context.finalize())
            .map_err(|e| anyhow!("Failed to serialize instructions to bytecode: {}", e))?;

        //std::fs::write(PathBuf::from("avm_bytecode.bin"), &bytecode_data)?;

        if log_enabled!(Level::Debug) {
            debug!("Serialized instructions to {} bytes of bytecode", bytecode_data.len());
        }

        // 创建全局字节数组常量
        let global_name = format!("__avm_bytecode_{}", rand::random::<u32>());
        let global_value = self.module.add_global(
            context.i8_type().array_type(bytecode_data.len() as u32),
            Some(AddressSpace::default()),
            &global_name,
        );

        // 设置初始值
        let byte_values: Vec<_> = bytecode_data
            .iter()
            .map(|&b| context.i8_type().const_int(b as u64, false))
            .collect();
        let array_value = context.i8_type().const_array(&byte_values);
        global_value.set_initializer(&array_value);
        global_value.set_constant(true);
        global_value.set_linkage(Linkage::Private);

        Ok(global_value)
    }

    /// 替换函数体为调用虚拟机的代码
    fn replace_function_body_with_vm_call(
        &self,
        function: FunctionValue,
        instructions_data: GlobalValue<'a>,
        runtime_functions: RuntimeFunctions,
    ) -> Result<()> {
        let context = self.module.get_context();
        let builder = context.create_builder();

        let target_function = unsafe {
            self.module
                .specialize_function_by_args(
                    (function.as_value_ref() as LLVMValueRef).into_function_value().unwrap(),
                    &[],
                )
                .map_err(|msg| anyhow!("Failed to backup original function: {}", msg))?
        };

        Ok(())
    }

    fn declare_runtime_functions(&self, vmp_context: VMPCompilerContext) -> Result<RuntimeFunctions<'a>> {
        let value_tag_map = self.encoder.get_value_type_map().clone();
        let opcode_map = self.encoder.get_opcode_map().clone();

        let context = self.module.get_context();

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        let i8_type = context.i8_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let ptr_type = context.ptr_type(AddressSpace::default());

        let vm_value_type = context.struct_type(
            &[
                i8_type.into(),  // type
                i64_type.into(), // value (using largest type for simplicity)
            ],
            false,
        );

        let vm_register_module = self.gen_vm_register_module(vm_value_type, vmp_context.get_max_register())?;
        let vm_stack_module = self.gen_vm_stack_module(vm_value_type)?;
        let vm_memory_module = self.gen_vm_memory_module()?;

        let builder = context.create_builder();

        // 声明 AVM runtime 函数
        let avm_runtime_new = {
            let avm_runtime_new_type = ptr_type.fn_type(&[], false); // 返回运行时实例指针
            let avm_runtime_new = self.module.add_function("avm_runtime_new", avm_runtime_new_type, None);
            avm_runtime_new.add_attribute(AttributeLoc::Function, inline_attr);

            avm_runtime_new
        };

        let avm_runtime_destroy_type = void_type.fn_type(&[ptr_type.into()], false);
        let avm_runtime_destroy = self
            .module
            .add_function("avm_runtime_destroy", avm_runtime_destroy_type, None);
        avm_runtime_destroy.add_attribute(AttributeLoc::Function, inline_attr);

        // avm_runtime_execute(runtime_ptr, bytecode_ptr, bytecode_length) -> i64
        let avm_runtime_execute = {
            let avm_runtime_execute_type =
                i64_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
            let avm_runtime_execute = self
                .module
                .add_function("avm_runtime_execute", avm_runtime_execute_type, None);
            avm_runtime_execute.add_attribute(AttributeLoc::Function, inline_attr);

            let entry = context.append_basic_block(avm_runtime_execute, "entry");
            builder.position_at_end(entry);

            builder.build_return(None)?;

            avm_runtime_destroy
        };

        Ok(RuntimeFunctions {
            avm_runtime_new,
            avm_runtime_destroy,
            avm_runtime_execute,
        })
    }

    fn gen_vm_stack_module(&self, vm_value_type: StructType<'a>) -> Result<VMStackModule<'a>> {
        let context = self.module.get_context();

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();

        let vm_value_ptr = vm_value_type.ptr_type(AddressSpace::default());

        // typedef struct {
        //     VMPValue *data;
        //     size_t len;
        //     size_t cap;
        // } ValueStack;
        let value_stack_type = context.struct_type(
            &[
                vm_value_ptr.into(), // data
                i64_type.into(),     // size
                i64_type.into(),     // capacity
            ],
            false,
        );
        let value_stack_ptr = value_stack_type.ptr_type(AddressSpace::default());

        let i32_zero = i32_type.const_zero();
        let i64_one = i64_type.const_int(1, false);

        // void avm_init_stack(ValueStack *stack);
        let function_type = void_type.fn_type(&[value_stack_ptr.into()], false);
        let avm_init_stack = self
            .module
            .add_function("avm_init_stack", function_type, Some(Linkage::Private));
        {
            avm_init_stack.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_init_stack
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_init_stack, "entry");
            builder.position_at_end(entry);

            let data_gep = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let len_gep = builder.build_struct_gep2(value_stack_type, param_stack, 1, "len_ptr")?;
            let cap_gep = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap_ptr")?;

            builder.build_store(data_gep, vm_value_ptr.const_null())?;
            builder.build_store(len_gep, i64_type.const_zero())?;
            builder.build_store(cap_gep, i64_type.const_zero())?;

            builder.build_return(None)?;
        }

        let function_type = void_type.fn_type(&[value_stack_ptr.into()], false);
        let avm_stack_destroy = self
            .module
            .add_function("avm_stack_free", function_type, Some(Linkage::Private));
        {
            avm_stack_destroy.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_stack_destroy
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_stack_destroy, "entry");
            builder.position_at_end(entry);
            let data_gep = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?; // &VMValue*
            let data_ptr = builder
                .build_load2(vm_value_ptr, data_gep, "data")?
                .into_pointer_value(); // VMValue*
            builder.build_free(data_ptr)?;

            let len_gep = builder.build_struct_gep2(value_stack_type, param_stack, 1, "len_ptr")?;
            builder.build_store(len_gep, i64_type.const_zero())?;

            let cap_gep = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap_ptr")?;
            builder.build_store(cap_gep, i64_type.const_zero())?;

            builder.build_return(None)?;
        }

        let function_type = void_type.fn_type(&[value_stack_ptr.into(), i64_type.into()], false);
        let avm_stack_reserve = self
            .module
            .add_function("avm_stack_reserve", function_type, Some(Linkage::Private));
        {
            avm_stack_reserve.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_stack_reserve
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();
            let param_need = avm_stack_reserve
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_int_value();

            let entry = context.append_basic_block(avm_stack_reserve, "entry");
            let return_bb = context.append_basic_block(avm_stack_reserve, "return");
            let then_bb = context.append_basic_block(avm_stack_reserve, "if.then");
            let end_bb = context.append_basic_block(avm_stack_reserve, "if.end");
            let cond_true_bb = context.append_basic_block(avm_stack_reserve, "cond.true");
            let cond_false_bb = context.append_basic_block(avm_stack_reserve, "cond.false");
            let cond_end_bb = context.append_basic_block(avm_stack_reserve, "cond.end");
            let while_cond_bb = context.append_basic_block(avm_stack_reserve, "while.cond");
            let while_body_bb = context.append_basic_block(avm_stack_reserve, "while.body");
            let while_end_bb = context.append_basic_block(avm_stack_reserve, "while.end");
            let null_data_bb = context.append_basic_block(avm_stack_reserve, "if.then6");
            let not_null_data_bb = context.append_basic_block(avm_stack_reserve, "if.end8");
            let merge = context.append_basic_block(avm_stack_reserve, "merge");

            builder.position_at_end(entry);
            let new_cap_ptr = builder.build_alloca(i64_type, "new_cap")?;
            let cap = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap_ptr")?;
            let cap_value = builder.build_load2(i64_type, cap, "cap")?.into_int_value();
            // if cap >= need return
            let cmp = builder.build_int_compare(IntPredicate::UGE, cap_value, param_need, "cmp")?;

            builder.build_conditional_branch(cmp, then_bb, end_bb)?;

            builder.position_at_end(then_bb);
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(end_bb);
            let cap1_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap1_ptr")?;
            let cap1_value = builder.build_load2(i64_type, cap1_ptr, "cap1")?.into_int_value();
            let is_nonzero =
                builder.build_int_compare(IntPredicate::NE, cap1_value, i64_type.const_zero(), "is_nonzero")?;

            builder.build_conditional_branch(is_nonzero, cond_true_bb, cond_false_bb)?;

            builder.position_at_end(cond_true_bb);
            let cap2_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap2_ptr")?;
            let cap2_value = builder.build_load2(i64_type, cap2_ptr, "cap2")?.into_int_value();
            builder.build_unconditional_branch(cond_end_bb)?;

            builder.position_at_end(cond_false_bb);
            builder.build_unconditional_branch(cond_end_bb)?;

            builder.position_at_end(cond_end_bb);
            let cond_phi = builder.build_phi(i64_type, "cond")?;
            cond_phi.add_incoming(&[
                (&cap2_value, cond_true_bb),
                (&i64_type.const_int(16, false), cond_false_bb),
            ]);
            builder.build_store(new_cap_ptr, cond_phi.as_basic_value().into_int_value())?;
            builder.build_unconditional_branch(while_cond_bb)?;

            builder.position_at_end(while_cond_bb);
            let new_cap_value = builder.build_load2(i64_type, new_cap_ptr, "new_cap")?.into_int_value();
            let cmp3 = builder.build_int_compare(IntPredicate::ULT, new_cap_value, param_need, "cmp3")?;
            builder.build_conditional_branch(cmp3, while_body_bb, while_end_bb)?;

            builder.position_at_end(while_body_bb);
            let mul = builder.build_int_mul(new_cap_value, i64_type.const_int(2, false), "mul")?;
            builder.build_store(new_cap_ptr, mul)?;
            builder.build_unconditional_branch(while_cond_bb)?;

            builder.position_at_end(while_end_bb);
            let data_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let data_value = builder
                .build_load2(vm_value_ptr, data_ptr, "data")?
                .into_pointer_value();
            let new_cap_final = builder
                .build_load2(i64_type, new_cap_ptr, "new_cap_final")?
                .into_int_value();
            let alloc_size = builder.build_int_mul(
                new_cap_final,
                i64_type.const_int(vm_value_type.as_basic_type_enum().size_in_bytes()? as u64, false),
                "alloc_size",
            )?;
            let new_data = builder.build_array_malloc(i8_type, alloc_size, "new_data")?;
            let cmp = builder.build_int_compare(IntPredicate::EQ, data_value, vm_value_ptr.const_null(), "cmp")?;
            builder.build_conditional_branch(cmp, null_data_bb, not_null_data_bb)?;

            builder.position_at_end(null_data_bb);
            let data_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            builder.build_store(data_ptr, new_data)?;
            builder.build_unconditional_branch(merge)?;

            builder.position_at_end(not_null_data_bb);
            let data_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let data_value = builder
                .build_load2(vm_value_ptr, data_ptr, "data")?
                .into_pointer_value();
            let cap_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap_ptr")?;
            let cap_value = builder.build_load2(i64_type, cap_ptr, "cap")?.into_int_value();
            builder.build_memcpy(new_data, 8, data_value, 8, cap_value)?;
            builder.build_free(data_value)?;
            builder.build_store(data_ptr, new_data)?;
            builder.build_unconditional_branch(merge)?;

            builder.position_at_end(merge);
            let cap_ptr = builder.build_struct_gep2(value_stack_type, param_stack, 2, "cap_ptr")?;
            let new_cap_final = builder
                .build_load2(i64_type, new_cap_ptr, "new_cap_final")?
                .into_int_value();
            builder.build_store(cap_ptr, new_cap_final)?;
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(return_bb);
            builder.build_return(None)?;
        }

        let function_type = void_type.fn_type(&[value_stack_ptr.into(), vm_value_type.into()], false);
        let avm_stack_push = self
            .module
            .add_function("avm_stack_push", function_type, Some(Linkage::Private));
        {
            avm_stack_push.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_stack_push
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let param_value = avm_stack_push
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_struct_value();

            let entry = context.append_basic_block(avm_stack_push, "entry");
            builder.position_at_end(entry);

            let val_ptr = builder.build_alloca(value_stack_type, "val_ptr")?;
            builder.build_store(val_ptr, param_value)?;
            let len_gep = builder.build_struct_gep2(value_stack_type, param_stack, 1, "len_ptr")?;
            let len_value = builder.build_load2(i64_type, len_gep, "len")?.into_int_value();
            let new_len = builder.build_int_add(len_value, i64_one, "new_len")?;

            // 调用 reserve
            builder.build_call(avm_stack_reserve, &[param_stack.into(), new_len.into()], "call_reserve")?;

            let data_gep = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let data_value = builder
                .build_load2(vm_value_ptr, data_gep, "data")?
                .into_pointer_value();

            let elem_ptr = builder.build_in_bounds_gep2(vm_value_type, data_value, &[len_value], "elem_ptr")?;

            builder.build_memmove(elem_ptr, 8, val_ptr, 8, vm_value_type.size_of().unwrap())?;

            builder.build_store(len_gep, new_len)?;
            builder.build_return(None)?;
        }

        let function_type = i32_type.fn_type(&[value_stack_ptr.into(), vm_value_ptr.into()], false);
        let avm_stack_pop = self
            .module
            .add_function("avm_stack_pop", function_type, Some(Linkage::Private));
        {
            avm_stack_pop.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_stack_pop
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let param_out = avm_stack_pop
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_stack_pop, "entry");
            let return_bb = context.append_basic_block(avm_stack_pop, "return");
            let then_bb = context.append_basic_block(avm_stack_pop, "if.then");
            let else_bb = context.append_basic_block(avm_stack_pop, "if.else");
            let end_bb = context.append_basic_block(avm_stack_pop, "if.end");

            builder.position_at_end(entry);
            let ret_val = builder.build_alloca(i32_type, "ret_val")?;
            let len_gep = builder.build_struct_gep2(value_stack_type, param_stack, 1, "len_ptr")?;
            let len_value = builder.build_load2(i64_type, len_gep, "len")?.into_int_value();
            let is_empty = builder.build_int_compare(IntPredicate::EQ, len_value, i64_type.const_zero(), "is_empty")?;

            builder.build_conditional_branch(is_empty, then_bb, else_bb)?;

            builder.position_at_end(then_bb);
            builder.build_store(
                ret_val,
                i32_type
                    .const_int_from_string("-1", StringRadix::Decimal)
                    .ok_or_else(|| anyhow!("Invalid return value"))?,
            )?;
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(else_bb);
            let new_len = builder.build_int_sub(len_value, i64_one, "new_len")?;
            let data_gep = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let data_value = builder
                .build_load2(vm_value_ptr, data_gep, "data")?
                .into_pointer_value();
            let elem_ptr = builder.build_in_bounds_gep2(vm_value_type, data_value, &[new_len], "elem_ptr")?;
            builder.build_memcpy(param_out, 8, elem_ptr, 8, vm_value_type.size_of().unwrap())?;
            builder.build_store(len_gep, new_len)?;
            builder.build_store(ret_val, i32_zero)?;
            builder.build_unconditional_branch(end_bb)?;

            builder.position_at_end(end_bb);
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(return_bb);
            let ret_val = builder.build_load2(i32_type, ret_val, "ret_val")?;
            builder.build_return(Some(&ret_val.into_int_value()))?;
        }

        let avm_stack_peek = self
            .module
            .add_function("avm_stack_peek", function_type, Some(Linkage::Private));
        {
            avm_stack_peek.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_stack = avm_stack_peek
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let param_out = avm_stack_peek
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_stack_peek, "entry");
            let return_bb = context.append_basic_block(avm_stack_peek, "return");
            let then_bb = context.append_basic_block(avm_stack_peek, "if.then");
            let else_bb = context.append_basic_block(avm_stack_peek, "if.else");
            let end_bb = context.append_basic_block(avm_stack_peek, "if.end");

            builder.position_at_end(entry);
            let ret_val = builder.build_alloca(i32_type, "ret_val")?;
            let len_gep = builder.build_struct_gep2(value_stack_type, param_stack, 1, "len_ptr")?;
            let len_value = builder.build_load2(i64_type, len_gep, "len")?.into_int_value();
            let is_empty = builder.build_int_compare(IntPredicate::EQ, len_value, i64_type.const_zero(), "is_empty")?;

            builder.build_conditional_branch(is_empty, then_bb, else_bb)?;

            builder.position_at_end(then_bb);
            builder.build_store(
                ret_val,
                i32_type
                    .const_int_from_string("-1", StringRadix::Decimal)
                    .ok_or_else(|| anyhow!("Invalid return value"))?,
            )?;
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(else_bb);
            let new_len = builder.build_int_sub(len_value, i64_one, "new_len")?;
            let data_gep = builder.build_struct_gep2(value_stack_type, param_stack, 0, "data_ptr")?;
            let data_value = builder
                .build_load2(vm_value_ptr, data_gep, "data")?
                .into_pointer_value();
            let elem_ptr = builder.build_in_bounds_gep2(vm_value_type, data_value, &[new_len], "elem_ptr")?;
            builder.build_memcpy(param_out, 8, elem_ptr, 8, vm_value_type.size_of().unwrap())?;
            builder.build_store(ret_val, i32_zero)?;
            builder.build_unconditional_branch(end_bb)?;

            builder.position_at_end(end_bb);
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(return_bb);
            let ret_val = builder.build_load2(i32_type, ret_val, "ret_val")?;
            builder.build_return(Some(&ret_val.into_int_value()))?;
        }

        Ok(VMStackModule {
            value_stack_type,
            init_stack: avm_init_stack,
            stack_destroy: avm_stack_destroy,
            stack_reserve: avm_stack_reserve,
            stack_push: avm_stack_push,
            stack_pop: avm_stack_pop,
            stack_peek: avm_stack_peek,
        })
    }

    fn gen_vm_register_module(
        &self,
        vm_value_type: StructType<'a>,
        max_register_id: u32,
    ) -> Result<VMVirtualRegisterModule<'a>> {
        let context = self.module.get_context();

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        let i32_type = context.i32_type();
        let void_type = context.void_type();

        let register_array_type = vm_value_type.array_type(max_register_id + 1);
        let register_array_ptr = register_array_type.ptr_type(AddressSpace::default());

        let i32_zero = i32_type.const_zero();

        // void avm_set_register_value(int64_t *register_array, int32_t reg_index, VMPValue value);
        let function_type = void_type.fn_type(
            &[register_array_ptr.into(), i32_type.into(), vm_value_type.into()],
            false,
        );
        let avm_set_register_value =
            self.module
                .add_function("avm_set_register_value", function_type, Some(Linkage::Private));
        {
            avm_set_register_value.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_register_array = avm_set_register_value
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();
            let param_reg_index = avm_set_register_value
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_int_value();
            let param_value = avm_set_register_value
                .get_nth_param(2)
                .ok_or_else(|| anyhow!("Missing parameter 2"))?
                .into_struct_value();

            let entry = context.append_basic_block(avm_set_register_value, "entry");

            builder.position_at_end(entry);

            let cond = builder.build_int_compare(
                IntPredicate::UGE,
                param_reg_index,
                i32_type.const_int(register_array_type.len() as u64, false),
                "reg_index_in_bounds",
            )?;
            let then_bb = context.append_basic_block(avm_set_register_value, "then");
            let else_bb = context.append_basic_block(avm_set_register_value, "else");
            builder.build_conditional_branch(cond, then_bb, else_bb)?;

            builder.position_at_end(then_bb);
            builder.build_unreachable()?;

            builder.position_at_end(else_bb);
            let reg_ptr = builder.build_in_bounds_gep2(
                register_array_type,
                param_register_array,
                &[i32_zero, param_reg_index],
                "reg_ptr",
            )?;
            builder.build_store(reg_ptr, param_value)?;

            builder.build_return(None)?;
        }

        Ok(VMVirtualRegisterModule {
            register_array_type,
            set_register_value: avm_set_register_value,
        })
    }

    fn gen_vm_memory_module(&self) -> Result<VMMemoryModule<'a>> {
        let context = self.module.get_context();

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);

        let i32_zero = i32_type.const_zero();

        let memory_type = context.struct_type(
            &[
                i8_ptr.into(),   // data (stack memory)
                i64_type.into(), // size
                i64_type.into(), // next_addr
            ],
            false,
        );
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());

        let function_type = void_type.fn_type(&[memory_ptr.into()], false);
        let avm_mem_init = self
            .module
            .add_function("avm_mem_init", function_type, Some(Linkage::Private));
        {
            avm_mem_init.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_memory = avm_mem_init
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_mem_init, "entry");
            builder.position_at_end(entry);

            let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
            let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
            let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;

            let data_ptr = builder.build_array_malloc(i8_type, i32_type.const_int(1024 * 1024, false), "data_ptr")?;

            builder.build_store(data_gep, data_ptr)?;
            builder.build_store(size_gep, i64_type.const_int(1024 * 1024, false))?;
            builder.build_store(next_addr_gep, i64_type.const_int(0x1000, false))?;
            builder.build_return(None)?;
        }

        let avm_mem_destroy = self
            .module
            .add_function("avm_mem_free", function_type, Some(Linkage::Private));
        {
            avm_mem_destroy.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_memory = avm_mem_destroy
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();

            let entry = context.append_basic_block(avm_mem_destroy, "entry");
            builder.position_at_end(entry);

            let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
            let data_value = builder.build_load2(i8_ptr, data_gep, "data")?.into_pointer_value();
            builder.build_free(data_value)?;

            let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
            builder.build_store(size_gep, i64_type.const_zero())?;

            let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;
            builder.build_store(next_addr_gep, i64_type.const_zero())?;

            builder.build_return(None)?;
        }

        let function_type = void_type.fn_type(&[memory_ptr.into(), i64_type.into()], false);
        let avm_mem_ensure = self
            .module
            .add_function("avm_mem_ensure", function_type, Some(Linkage::Private));
        {
            avm_mem_ensure.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_memory = avm_mem_ensure
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();
            let param_need = avm_mem_ensure
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_int_value();

            let entry = context.append_basic_block(avm_mem_ensure, "entry");
            let return_bb = context.append_basic_block(avm_mem_ensure, "return");
            let then_bb = context.append_basic_block(avm_mem_ensure, "if.then");
            let end_bb = context.append_basic_block(avm_mem_ensure, "if.end");
            let while_cond_bb = context.append_basic_block(avm_mem_ensure, "while.cond");
            let while_body_bb = context.append_basic_block(avm_mem_ensure, "while.body");
            let while_end_bb = context.append_basic_block(avm_mem_ensure, "while.end");

            builder.position_at_end(entry);
            let new_size_ptr = builder.build_alloca(i64_type, "new_size")?;
            let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
            let size_value = builder.build_load2(i64_type, size_gep, "size")?.into_int_value();
            // if (need <= m->size) return;
            let cmp = builder.build_int_compare(IntPredicate::ULE, param_need, size_value, "cmp")?;
            builder.build_conditional_branch(cmp, then_bb, end_bb)?;

            builder.position_at_end(then_bb);
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(end_bb);
            builder.build_store(new_size_ptr, size_value)?;
            builder.build_unconditional_branch(while_cond_bb)?;

            builder.position_at_end(while_cond_bb);
            let new_size_value = builder
                .build_load2(i64_type, new_size_ptr, "new_size")?
                .into_int_value();
            let cmp = builder.build_int_compare(IntPredicate::ULT, new_size_value, param_need, "cmp")?;
            builder.build_conditional_branch(cmp, while_body_bb, while_end_bb)?;

            builder.position_at_end(while_body_bb);
            let div = builder.build_int_unsigned_div(new_size_value, i64_type.const_int(2, false), "")?;
            let add = builder.build_int_add(div, i64_type.const_int(4096, false), "add")?;
            let add = builder.build_int_add(new_size_value, add, "add")?;
            builder.build_store(new_size_ptr, add)?;
            builder.build_unconditional_branch(while_cond_bb)?;

            builder.position_at_end(while_end_bb);
            let old_data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
            let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
            let old_data = builder.build_load2(i8_ptr, old_data_gep, "data")?.into_pointer_value();
            let new_data = builder.build_array_malloc(i8_type, new_size_value, "new_data")?;
            builder.build_memcpy(new_data, 8, old_data, 8, size_value)?;
            builder.build_free(old_data)?;
            builder.build_store(old_data_gep, new_data)?;
            let new_size_value = builder
                .build_load2(i64_type, new_size_ptr, "new_size")?
                .into_int_value();
            builder.build_store(size_gep, new_size_value)?;
            builder.build_unconditional_branch(return_bb)?;

            builder.position_at_end(return_bb);
            builder.build_return(None)?;
        }

        let function_type = i64_type.fn_type(&[memory_ptr.into(), i64_type.into()], false);
        let avm_mem_alloc = self.module.add_function("avm_mem_alloc", function_type, None);
        {
            avm_mem_alloc.add_attribute(AttributeLoc::Function, inline_attr);

            let builder = context.create_builder();

            let param_memory = avm_mem_alloc
                .get_nth_param(0)
                .ok_or_else(|| anyhow!("Missing parameter 0"))?
                .into_pointer_value();
            let param_payload_size = avm_mem_alloc
                .get_nth_param(1)
                .ok_or_else(|| anyhow!("Missing parameter 1"))?
                .into_int_value();

            let entry = context.append_basic_block(avm_mem_alloc, "entry");
            builder.position_at_end(entry);
            let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;
            let next_addr_value = builder
                .build_load2(i64_type, next_addr_gep, "next_addr")?
                .into_int_value();
            let need = builder.build_int_add(next_addr_value, param_payload_size, "need")?;
            let need_plus_1024 =
                builder.build_int_add(next_addr_value, i64_type.const_int(1024, false), "need_plus_1024")?;

            builder.build_call(
                avm_mem_ensure,
                &[param_memory.into(), need_plus_1024.into()],
                "call_mem_ensure",
            )?;

            builder.build_store(next_addr_gep, need)?;

            builder.build_return(Some(&next_addr_value))?;
        }

        Ok(VMMemoryModule {
            memory_type,
            mem_init: avm_mem_init,
            mem_destroy: avm_mem_destroy,
            mem_ensure: avm_mem_ensure,
            mem_alloc: avm_mem_alloc,
        })
    }
}

struct VMStackModule<'a> {
    value_stack_type: StructType<'a>,
    init_stack: FunctionValue<'a>,
    stack_destroy: FunctionValue<'a>,
    stack_reserve: FunctionValue<'a>,
    stack_push: FunctionValue<'a>,
    stack_pop: FunctionValue<'a>,
    stack_peek: FunctionValue<'a>,
}

struct VMVirtualRegisterModule<'a> {
    register_array_type: ArrayType<'a>,
    set_register_value: FunctionValue<'a>,
}

struct VMMemoryModule<'a> {
    memory_type: StructType<'a>,
    mem_init: FunctionValue<'a>,
    mem_destroy: FunctionValue<'a>,
    mem_ensure: FunctionValue<'a>,
    mem_alloc: FunctionValue<'a>,
}

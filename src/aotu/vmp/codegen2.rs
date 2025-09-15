#![allow(deprecated)]

use crate::aotu::vmp::bytecode::{BytecodeOp, BytecodeValueType, VMPBytecodeEncoder};
use crate::aotu::vmp::compiler::VMPCompilerContext;
use amice_llvm::inkwell2::{BuilderExt, LLVMValueRefExt};
use amice_llvm::ptr_type;
use anyhow::{anyhow, Result};
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::{ArrayType, BasicType, BasicTypeEnum, StringRadix, StructType};
use llvm_plugin::inkwell::values::{AsValueRef, BasicValueEnum, FunctionValue, GlobalValue, IntValue, PointerValue};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use std::collections::HashMap;

const AVM_MEM_DEFAULT_SIZE: u64 = 1024 * 1024;
const AVM_PAGE_SIZE: u64 = 4096;
const AVM_ALLOC_SLACK: u64 = 1024;
const DEFAULT_STACK_CAPACITY: u64 = 16;

const DEFAULT_LINKAGE: Option<Linkage> = None;

/// VM代码生成器主类
pub struct VMPCodeGenerator<'a, 'b> {
    module: &'b mut Module<'a>,
    encoder: VMPBytecodeEncoder,
    codegen_helper: CodeGenHelper<'a>,
}

/// 类型和常量的统一管理
struct CodeGenHelper<'a> {
    context: ContextRef<'a>,
    vm_value_type: StructType<'a>,
    inline_attr: Attribute,
}

impl<'a> CodeGenHelper<'a> {
    fn new(context: ContextRef<'a>, module: &mut Module) -> Self {
        let vm_value_type = context.struct_type(
            &[
                context.i8_type().into(),  // type tag
                context.i64_type().into(), // value union
            ],
            false,
        );

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        Self {
            context,
            vm_value_type,
            inline_attr,
        }
    }
}

/// 运行时函数集合
#[derive(Copy, Clone)]
struct RuntimeModule<'a> {
    runtime_type: StructType<'a>,
    get_value_size_by_tag: FunctionValue<'a>,
    runtime_new: FunctionValue<'a>,
    runtime_destroy: FunctionValue<'a>,
    runtime_execute: FunctionValue<'a>,
    stack_module: VMStackModule<'a>,
    register_module: VMRegisterModule<'a>,
    memory_module: VMMemoryModule<'a>,
}

/// VM栈模块
#[derive(Copy, Clone)]
struct VMStackModule<'a> {
    stack_type: StructType<'a>,
    functions: StackFunctions<'a>,
}

#[derive(Copy, Clone)]
struct StackFunctions<'a> {
    init: FunctionValue<'a>,
    destroy: FunctionValue<'a>,
    reserve: FunctionValue<'a>,
    push: FunctionValue<'a>,
    pop: FunctionValue<'a>,
    peek: FunctionValue<'a>,
}

/// VM寄存器模块
#[derive(Copy, Clone)]
struct VMRegisterModule<'a> {
    array_type: ArrayType<'a>,
    set_value: FunctionValue<'a>,
    get_value: FunctionValue<'a>,
}

/// VM内存模块
#[derive(Copy, Clone)]
struct VMMemoryModule<'a> {
    memory_type: StructType<'a>,
    functions: MemoryFunctions<'a>,
}

#[derive(Copy, Clone)]
struct MemoryFunctions<'a> {
    init: FunctionValue<'a>,
    destroy: FunctionValue<'a>,
    ensure: FunctionValue<'a>,
    alloc: FunctionValue<'a>,
    store_value: FunctionValue<'a>,
    load_value: FunctionValue<'a>,
}

impl<'a, 'b> VMPCodeGenerator<'a, 'b> {
    pub fn new(module: &'b mut Module<'a>) -> Result<Self> {
        let context = module.get_context();
        let codegen_helper = CodeGenHelper::new(context, module);

        Ok(Self {
            module,
            encoder: VMPBytecodeEncoder::new(),
            codegen_helper,
        })
    }

    /// 编译函数为VM调用
    pub fn compile_function_to_vm_call(&mut self, function: FunctionValue, context: VMPCompilerContext) -> Result<()> {
        let instructions_data = self.serialize_instructions(&context)?;
        let runtime_functions = self.create_runtime_functions(context)?;
        //self.replace_function_body(function, instructions_data, runtime_functions)?;
        Ok(())
    }

    /// 序列化指令到全局常量
    fn serialize_instructions(&mut self, compiler_context: &VMPCompilerContext) -> Result<GlobalValue<'a>> {
        let context = self.module.get_context();
        let i8_type = context.i8_type();

        let bytecode_data = self
            .encoder
            .encode_instructions(compiler_context.finalize())
            .map_err(|e| anyhow!("Failed to serialize instructions: {}", e))?;

        let global_name = format!("__avm_bytecode_{}", rand::random::<u32>());
        let array_type = i8_type.array_type(bytecode_data.len() as u32);

        let global_value = self
            .module
            .add_global(array_type, Some(AddressSpace::default()), &global_name);

        let byte_values: Vec<_> = bytecode_data
            .iter()
            .map(|&b| i8_type.const_int(b as u64, false))
            .collect();
        let array_value = i8_type.const_array(&byte_values);

        global_value.set_initializer(&array_value);
        global_value.set_constant(true);
        global_value.set_linkage(Linkage::Private);

        Ok(global_value)
    }

    /// 创建所有运行时函数
    fn create_runtime_functions(&mut self, vmp_context: VMPCompilerContext) -> Result<RuntimeModule<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();

        let get_value_size_by_tag = self.create_get_value_size_by_tag()?;

        let stack_module = StackModuleBuilder::new(&self.codegen_helper).build(self.module)?;
        let register_module =
            RegisterModuleBuilder::new(&self.codegen_helper, vmp_context.get_max_register()).build(self.module)?;
        let memory_module = MemoryModuleBuilder::new(&self.codegen_helper).build(
            self.module,
            self.encoder.get_value_type_map(),
            get_value_size_by_tag,
        )?;

        let runtime_type = self.codegen_helper.context.struct_type(
            &[
                stack_module.stack_type.into(),    // stack
                register_module.array_type.into(), // registers
                memory_module.memory_type.into(),  // memory
                i64_type.into(),                   // PC
            ],
            false,
        );

        // 创建主要的运行时函数
        let avm_runtime_new = self.create_runtime_new(
            runtime_type,
            stack_module.functions.init,
            vmp_context.get_max_register(),
            memory_module.functions.init,
        )?;
        let avm_runtime_destroy = self.create_runtime_destroy(
            runtime_type,
            stack_module.functions.destroy,
            memory_module.functions.destroy,
        )?;
        let avm_runtime_execute = self.create_runtime_execute(runtime_type, get_value_size_by_tag, stack_module, register_module, memory_module)?;

        Ok(RuntimeModule {
            runtime_type,
            get_value_size_by_tag,
            runtime_new: avm_runtime_new,
            runtime_destroy: avm_runtime_destroy,
            runtime_execute: avm_runtime_execute,
            stack_module,
            register_module,
            memory_module,
        })
    }

    #[inline]
    fn get_runtime_stack_ptr(
        &self,
        builder: &Builder<'a>,
        runtime_type: StructType<'a>,
        runtime_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>> {
        let stack_ptr = builder.build_struct_gep2(runtime_type, runtime_ptr, 0, "stack_ptr")?;
        Ok(stack_ptr)
    }

    #[inline]
    fn get_runtime_registers_ptr(
        &self,
        builder: &Builder<'a>,
        runtime_type: StructType<'a>,
        runtime_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>> {
        let registers_ptr = builder.build_struct_gep2(runtime_type, runtime_ptr, 1, "registers_ptr")?;
        Ok(registers_ptr)
    }

    #[inline]
    fn get_runtime_memory_ptr(
        &self,
        builder: &Builder<'a>,
        runtime_type: StructType<'a>,
        runtime_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>> {
        let memory_ptr = builder.build_struct_gep2(runtime_type, runtime_ptr, 2, "memory_ptr")?;
        Ok(memory_ptr)
    }

    fn create_get_value_size_by_tag(&self) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;

        let i8_type = context.i8_type();
        let i32_type = context.i32_type();

        let fn_type = i32_type.fn_type(&[i8_type.into()], false);
        let function = self
            .module
            .add_function("avm_get_value_size_by_tag", fn_type, DEFAULT_LINKAGE);

        let builder = context.create_builder();

        let entry = context.append_basic_block(function, "entry");
        let default_bb = context.append_basic_block(function, "default");

        let tag_param = function.get_nth_param(0).unwrap().into_int_value();

        builder.position_at_end(entry);
        let size_ptr = builder.build_alloca(i32_type, "size")?;

        // 默认大小为8字节（适用于大多数类型）
        builder.build_store(size_ptr, i32_type.const_int(8, false))?;

        // 对于特殊大小的类型进行判断
        let tag_i32 = builder.build_int_z_extend(tag_param, i32_type, "tag_i32")?;

        let mut cases = Vec::new();
        for (typ, val) in self.encoder.get_value_type_map() {
            let case_bb = context.append_basic_block(function, &format!("case_{:?}", typ));
            cases.push((i32_type.const_int(*val as u64, false), case_bb));
            builder.position_at_end(case_bb);
            builder.build_store(size_ptr, i32_type.const_int(typ.size() as u64, false))?;
            builder.build_unconditional_branch(default_bb)?;
        }
        builder.position_at_end(entry);
        builder.build_switch(tag_i32, default_bb, &cases)?;

        builder.position_at_end(default_bb);
        let final_size = builder.build_load2(i32_type, size_ptr, "final_size")?.into_int_value();

        builder.build_return(Some(&final_size))?;

        Ok(function)
    }

    fn create_runtime_new(
        &self,
        runtime_type: StructType<'a>,
        init_stack: FunctionValue<'a>,
        max_reg_num: u32,
        memory_init: FunctionValue<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i8_type = context.i8_type();
        let i64_type = context.i64_type();
        let runtime_ptr = runtime_type.ptr_type(AddressSpace::default());
        let fn_type = runtime_ptr.fn_type(&[], false);

        let function = self.module.add_function("avm_runtime_new", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        // 分配运行时结构
        let runtime_instance = builder.build_malloc(runtime_type, "runtime")?;

        // 初始化栈
        let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_instance)?;
        builder.build_call(init_stack, &[stack_ptr.into()], "")?;

        // 初始化寄存器数组（清零）
        let registers_ptr = self.get_runtime_registers_ptr(&builder, runtime_type, runtime_instance)?;
        let registers_size = self.codegen_helper.vm_value_type.size_of().unwrap();
        let total_reg_size = builder.build_int_mul(
            registers_size,
            i64_type.const_int((max_reg_num + 1) as u64, false),
            "total_reg_size",
        )?;
        builder.build_memset(registers_ptr, 8, i8_type.const_zero(), total_reg_size)?;

        // 初始化内存管理器
        let memory_ptr = self.get_runtime_memory_ptr(&builder, runtime_type, runtime_instance)?;
        builder.build_call(memory_init, &[memory_ptr.into()], "")?;

        let pc_ptr = builder.build_struct_gep2(runtime_type, runtime_instance, 3, "pc_ptr")?;
        builder.build_store(pc_ptr, i64_type.const_zero())?;

        builder.build_return(Some(&runtime_instance))?;

        Ok(function)
    }

    fn create_runtime_destroy(
        &self,
        runtime_type: StructType<'a>,
        stack_destroy: FunctionValue<'a>,
        memory_destroy: FunctionValue<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let void_type = context.void_type();
        let runtime_ptr = runtime_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[runtime_ptr.into()], false);
        let function = self
            .module
            .add_function("avm_runtime_destroy", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");

        builder.position_at_end(entry);
        let param_runtime = function.get_nth_param(0).unwrap().into_pointer_value();

        let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, param_runtime)?;
        builder.build_call(stack_destroy, &[stack_ptr.into()], "")?;

        let memory_ptr = self.get_runtime_memory_ptr(&builder, runtime_type, param_runtime)?;
        builder.build_call(memory_destroy, &[memory_ptr.into()], "")?;

        builder.build_free(param_runtime)?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_runtime_execute(&self, runtime_type: StructType<'a>, get_value_size_by_tag: FunctionValue, stack_module: VMStackModule, register_module: VMRegisterModule, memory_module: VMMemoryModule) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;

        let i8_type = context.i8_type();
        let i16_type = context.i16_type();
        let i64_type = context.i64_type();
        let i8_ptr = ptr_type!(context, i8_type);
        let i16_ptr = ptr_type!(context, i16_type);

        let runtime_ptr = runtime_type.ptr_type(AddressSpace::default());

        let fn_type = i64_type.fn_type(
            &[
                runtime_ptr.into(),
                i8_ptr.into(),   // bytecode pointer
                i64_type.into(), // bytecode length
            ],
            false,
        );

        let builder = context.create_builder();

        let function = self.module.add_function("avm_runtime_execute", fn_type, None);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        // 创建基本块
        let entry = context.append_basic_block(function, "entry");
        let main_loop = context.append_basic_block(function, "main_loop");
        let loop_body = context.append_basic_block(function, "loop_body");
        let return_block = context.append_basic_block(function, "return");

        // 为每个操作码创建基本块
        let mut opcode_blocks = HashMap::new();
        for (op, &opcode_id) in self.encoder.get_opcode_map() {
            let block_name = format!("op_{:?}", op);
            let block = context.append_basic_block(function, &block_name);
            opcode_blocks.insert(opcode_id, block);
        }
        let invalid_opcode_block = context.append_basic_block(function, "invalid_opcode");

        // 获取参数
        let runtime_ptr = function.get_nth_param(0).unwrap().into_pointer_value();
        let bytecode_ptr = function.get_nth_param(1).unwrap().into_pointer_value();
        let bytecode_len = function.get_nth_param(2).unwrap().into_int_value();

        // Entry块：初始化程序计数器
        builder.position_at_end(entry);
        let pc_ptr = builder.build_struct_gep2(runtime_type, runtime_ptr, 3, "pc_ptr")?;
        builder.build_store(pc_ptr, i64_type.const_zero())?;
        builder.build_unconditional_branch(main_loop)?;

        // 主循环：检查PC是否越界
        builder.position_at_end(main_loop);
        let pc_value = builder.build_load2(i64_type, pc_ptr, "pc_val")?.into_int_value();
        let loop_condition = builder.build_int_compare(IntPredicate::ULT, pc_value, bytecode_len, "in_bounds")?;
        builder.build_conditional_branch(loop_condition, loop_body, return_block)?;

        // 循环体：读取操作码并分发
        builder.position_at_end(loop_body);
        {
            let pc_value = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();

            // 读取操作码 (u16)
            let opcode_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc_value], "opcode_ptr")?;
            let opcode_i16_ptr = builder
                .build_bit_cast(opcode_ptr, i16_ptr, "opcode_i16_ptr")?
                .into_pointer_value();
            let opcode = builder
                .build_load2(i16_type, opcode_i16_ptr, "opcode")?
                .into_int_value();

            // PC += 2 (操作码大小)
            let pc_plus_2 = builder.build_int_add(pc_value, i64_type.const_int(2, false), "pc_plus_2")?;
            builder.build_store(pc_ptr, pc_plus_2)?;

            // 创建switch分发到不同的操作码处理器
            let mut cases = Vec::new();
            for (opcode_id, block) in &opcode_blocks {
                cases.push((i16_type.const_int(*opcode_id as u64, false), *block));
            }

            builder.build_switch(opcode, invalid_opcode_block, &cases)?;
        }

        // 无效操作码处理
        builder.position_at_end(invalid_opcode_block);
        builder.build_return(Some(&i64_type.const_int(u64::MAX, false)))?;

        // 为每个操作码实现具体逻辑
        self.implement_opcode_handlers(&builder, &opcode_blocks, runtime_type, runtime_ptr, bytecode_ptr, pc_ptr, main_loop, get_value_size_by_tag, stack_module, register_module, memory_module)?;

        // 返回块
        builder.position_at_end(return_block);
        builder.build_return(Some(&i64_type.const_zero()))?;

        Ok(function)
    }

    fn implement_opcode_handlers(
        &self,
        builder: &Builder<'a>,
        opcode_blocks: &HashMap<u16, BasicBlock<'a>>,
        runtime_type: StructType<'a>,
        runtime_ptr: PointerValue<'a>,
        bytecode_ptr: PointerValue<'a>,
        pc_ptr: PointerValue<'a>,
        main_loop: BasicBlock<'a>,
        get_value_size_by_tag: FunctionValue,
        stack_module: VMStackModule,
        register_module: VMRegisterModule,
        memory_module: VMMemoryModule,
    ) -> Result<()> {
        let context = self.codegen_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();

        let i32_ptr = ptr_type!(context, i32_type);

        let vm_value_type = self.codegen_helper.vm_value_type;

        let i32_zero = i32_type.const_zero();
        let i32_one = i32_type.const_int(1, false);
        let i64_one = i64_type.const_int(1, false);
        let i64_four = i64_type.const_int(4, false);

        for (op, &opcode_id) in self.encoder.get_opcode_map() {
            let block = opcode_blocks[&opcode_id];
            builder.position_at_end(block);

            match op {
                BytecodeOp::Push => {
                    let pc = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();
                    let type_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc], "type_ptr")?;
                    let type_tag = builder.build_load2(i8_type, type_ptr, "type_tag")?.into_int_value();

                    // PC += 1
                    let pc_plus_1 = builder.build_int_add(pc, i64_one, "pc_plus_1")?;
                    builder.build_store(pc_ptr, pc_plus_1)?;

                    // 根据类型读取值并推入栈
                    let dest_value_ptr = builder.build_alloca(vm_value_type, "")?;
                    // 清零
                    builder.build_memset(dest_value_ptr, 8, i8_type.const_zero(), vm_value_type.size_of().unwrap())?;
                    let value_tag_ptr = builder.build_struct_gep2(
                        vm_value_type,
                        dest_value_ptr,
                        0,
                        "value_tag_ptr",
                    )?;
                    let value_union_ptr = builder.build_struct_gep2(
                        vm_value_type,
                        dest_value_ptr,
                        1,
                        "value_union_ptr",
                    )?;

                    // 存储类型标签
                    builder.build_store(value_tag_ptr, type_tag)?;

                    let value_size = builder.build_call(
                        get_value_size_by_tag,
                        &[type_tag.into()],
                        "value_size",
                    )?.try_as_basic_value().left().unwrap().into_int_value();
                    let value_size = builder.build_int_z_extend(value_size, i64_type, "value_size_64")?;
                    let pc = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();
                    let value_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc], "value_ptr")?;
                    builder.build_memcpy(
                        value_union_ptr,
                        8,
                        value_ptr,
                        8,
                        value_size
                    )?;
                    // PC += value_size
                    let pc_plus_value = builder.build_int_add(pc, value_size, "pc_plus_value")?;
                    builder.build_store(pc_ptr, pc_plus_value)?;

                    let value = builder.build_load2(vm_value_type, value_ptr, "value")?
                        .into_struct_value();
                    let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_ptr)?;
                    let stack_push = stack_module.functions.push;
                    builder.build_call(stack_push, &[
                        stack_ptr.into(),
                        value.into()
                    ], "stack_push")?;

                    builder.build_unconditional_branch(main_loop)?;
                },
                BytecodeOp::PushFromReg => {
                    let success_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "get_success");
                    let fail_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "get_fail");

                    let pc = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();
                    let reg_index_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc], "reg_index_ptr")?;
                    let reg_index_ptr = builder.build_bit_cast(reg_index_ptr, i32_ptr, "")?.into_pointer_value();
                    let reg_index = builder.build_load2(i32_type, reg_index_ptr, "reg_index")?.into_int_value();

                    // PC += 4
                    let pc_plus_4 = builder.build_int_add(pc, i64_four, "pc_plus_1")?;
                    builder.build_store(pc_ptr, pc_plus_4)?;

                    // 从寄存器读取值并推入栈
                    let registers_ptr = self.get_runtime_registers_ptr(&builder, runtime_type, runtime_ptr)?;
                    let get_reg = register_module.get_value;
                    let out = builder.build_alloca(vm_value_type, "reg_value")?;
                    let result = builder.build_call(get_reg, &[registers_ptr.into(), reg_index.into(), out.into()], "set_reg")?;
                    let success = builder.build_int_compare(IntPredicate::EQ, result.try_as_basic_value().left().unwrap().into_int_value(), i32_type.const_zero(), "get_success")?;
                    builder.build_conditional_branch(success, success_block, fail_block)?;

                    // 成功分支
                    builder.position_at_end(success_block);
                    let reg_value = builder.build_load2(vm_value_type, out, "reg_value")?.into_struct_value();
                    let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_ptr)?;
                    let stack_push = stack_module.functions.push;
                    builder.build_call(stack_push, &[
                        stack_ptr.into(),
                        reg_value.into()
                    ], "stack_push")?;

                    // 跳转回主循环
                    builder.build_unconditional_branch(main_loop)?;

                    // 失败分支
                    builder.position_at_end(fail_block);
                    // 失败时返回错误码
                    builder.build_unreachable()?;
                },
                BytecodeOp::Pop => {
                    let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_ptr)?;
                    let vm_value_temp = builder.build_alloca(vm_value_type, "pop_value")?;
                    let pop_result = builder.build_call(
                        stack_module.functions.pop,
                        &[stack_ptr.into(), vm_value_temp.into()],
                        "pop_result",
                    )?;
                    let success = builder.build_int_compare(
                        IntPredicate::EQ,
                        pop_result.try_as_basic_value().left().unwrap().into_int_value(),
                        i32_type.const_zero(),
                        "pop_success",
                    )?;
                    let continue_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "continue");
                    let error_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "pop_error");
                    builder.build_conditional_branch(success, continue_block, error_block)?;

                    // 成功继续
                    builder.position_at_end(continue_block);
                    builder.build_unconditional_branch(main_loop)?;

                    // 失败处理
                    builder.position_at_end(error_block);
                    builder.build_unreachable()?;
                },
                BytecodeOp::PopToReg => {
                    let success_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "pop_success");
                    let fail_block = context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "pop_fail");

                    let pc = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();
                    let reg_index_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc], "reg_index_ptr")?;
                    let reg_index_ptr = builder.build_bit_cast(reg_index_ptr, i32_ptr, "")?.into_pointer_value();
                    let reg_index = builder.build_load2(i32_type, reg_index_ptr, "reg_index")?.into_int_value();

                    // PC += 4
                    let pc_plus_4 = builder.build_int_add(pc, i64_four, "pc_plus_4")?;
                    builder.build_store(pc_ptr, pc_plus_4)?;

                    // 从栈顶弹出值并存入寄存器
                    let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_ptr)?;
                    let vm_value_temp = builder.build_alloca(vm_value_type, "pop_value")?;
                    let pop_result = builder.build_call(
                        stack_module.functions.pop,
                        &[stack_ptr.into(), vm_value_temp.into()],
                        "pop_result",
                    )?;
                    let success = builder.build_int_compare(
                        IntPredicate::EQ,
                        pop_result.try_as_basic_value().left().unwrap().into_int_value(),
                        i32_type.const_zero(),
                        "pop_success",
                    )?;
                    builder.build_conditional_branch(success, success_block, fail_block)?;

                    // 成功分支
                    builder.position_at_end(success_block);
                    let registers_ptr = self.get_runtime_registers_ptr(&builder, runtime_type, runtime_ptr)?;
                    let set_reg = register_module.set_value;
                    builder.build_call(set_reg, &[registers_ptr.into(), reg_index.into(), vm_value_temp.into()], "set_reg")?;
                    builder.build_unconditional_branch(main_loop)?;

                    // 失败分支
                    builder.position_at_end(fail_block);
                    // 失败时返回错误码
                    builder.build_unreachable()?;
                },
                BytecodeOp::Alloca => {
                    let alloc = memory_module.functions.alloc;
                    let memory_ptr = self.get_runtime_memory_ptr(&builder, runtime_type, runtime_ptr)?;
                    let pc = builder.build_load2(i64_type, pc_ptr, "pc")?.into_int_value();
                    let size_ptr = builder.build_in_bounds_gep2(i8_type, bytecode_ptr, &[pc], "size_ptr")?;
                    let size_ptr = builder.build_bit_cast(size_ptr, i32_ptr, "")?.into_pointer_value();
                    let size = builder.build_load2(i32_type, size_ptr, "size")?.into_int_value();
                    // PC += 4
                    let pc_plus_4 = builder.build_int_add(pc, i64_four, "pc_plus_4")?;
                    builder.build_store(pc_ptr, pc_plus_4)?;

                    let size = builder.build_int_z_extend(size, i64_type, "size_64")?;
                    let size = builder.build_int_add(size, i64_four, "size")?;
                    let alloc_result = builder.build_call(alloc, &[memory_ptr.into(), size.into()], "alloc_result")?;
                    let addr = alloc_result.try_as_basic_value().left().unwrap().into_int_value();
                    let addr = builder.build_int_z_extend(addr, i64_type, "addr_64")?;
                    let addr = builder.build_int_add(addr, i64_one, "addr")?; // 地址偏移1，避免0地址
                    let dest_value_ptr = builder.build_alloca(vm_value_type, "")?;
                    // 清零
                    builder.build_memset(dest_value_ptr, 8, i8_type.const_zero(), vm_value_type.size_of().unwrap())?;
                    let value_tag_ptr = builder.build_struct_gep2(
                        vm_value_type,
                        dest_value_ptr,
                        0,
                        "value_tag_ptr",
                    )?;
                    let value_union_ptr = builder.build_struct_gep2(
                        vm_value_type,
                        dest_value_ptr,
                        1,
                        "value_union_ptr",
                    )?;
                    // 存储类型标签
                    builder.build_store(value_tag_ptr, i8_type.const_int(self.encoder.get_value_type_map()[&BytecodeValueType::Ptr] as u64, false))?;
                    // 存储地址
                    builder.build_store(value_union_ptr, addr)?;
                    let value = builder.build_load2(vm_value_type, dest_value_ptr, "value")?
                        .into_struct_value();
                    let stack_ptr = self.get_runtime_stack_ptr(&builder, runtime_type, runtime_ptr)?;
                    let stack_push = stack_module.functions.push;
                    builder.build_call(stack_push, &[
                        stack_ptr.into(),
                        value.into()
                    ], "stack_push")?;
                    builder.build_unconditional_branch(main_loop)?;
                },
                BytecodeOp::Nop => {
                    // NOP指令，直接跳转回主循环
                    builder.build_unconditional_branch(main_loop)?;
                },
                _ => {
                    // 其他指令暂时跳过
                    builder.build_unconditional_branch(main_loop)?;
                },
            }
        }

        Ok(())
    }

    fn replace_function_body(
        &self,
        function: FunctionValue,
        instructions_data: GlobalValue<'a>,
        runtime_module: RuntimeModule,
    ) -> Result<()> {
        let context = self.codegen_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let i8_ptr = ptr_type!(context, i8_type);

        // 清空原有函数体
        for x in function.get_basic_blocks() {
            unsafe {
                if let Err(_) = x.delete() {
                    return Err(anyhow!("Failed to delete basic block"));
                }
            }
        }

        // 创建新的入口块
        let entry = context.append_basic_block(function, "vmp_entry");
        let setup_runtime = self
            .codegen_helper
            .context
            .append_basic_block(function, "setup_runtime");
        let execute_vm = context.append_basic_block(function, "execute_vm");
        let cleanup_runtime = self
            .codegen_helper
            .context
            .append_basic_block(function, "cleanup_runtime");
        let return_block = context.append_basic_block(function, "return");
        let error_block = context.append_basic_block(function, "error");

        let builder = context.create_builder();

        // Entry块：分配运行时实例
        builder.position_at_end(entry);

        // 调用avm_runtime_new创建运行时实例
        let runtime_instance = builder.build_call(runtime_module.runtime_new, &[], "runtime_instance")?;
        let runtime_ptr = runtime_instance
            .try_as_basic_value()
            .left()
            .ok_or_else(|| anyhow!("Failed to get runtime instance"))?
            .into_pointer_value();

        // 检查runtime创建是否成功
        let runtime_null_check = builder.build_int_compare(
            IntPredicate::NE,
            runtime_ptr,
            runtime_module
                .runtime_type
                .ptr_type(AddressSpace::default())
                .const_null(),
            "runtime_not_null",
        )?;
        builder.build_conditional_branch(runtime_null_check, setup_runtime, error_block)?;

        // Setup Runtime块：准备字节码和参数
        builder.position_at_end(setup_runtime);

        // 获取字节码指针和长度
        let bytecode_ptr = builder
            .build_bit_cast(instructions_data.as_pointer_value(), i8_ptr, "bytecode_ptr")?
            .into_pointer_value();

        // 计算字节码长度
        let bytecode_len = self.calculate_bytecode_length(&builder, &instructions_data)?;

        // TODO 处理函数参数 - 将原函数参数传递给VM

        builder.build_unconditional_branch(execute_vm)?;

        // Execute VM块：执行虚拟机
        builder.position_at_end(execute_vm);

        let execute_result = builder.build_call(
            runtime_module.runtime_execute,
            &[runtime_ptr.into(), bytecode_ptr.into(), bytecode_len.into()],
            "execute_result",
        )?;

        let result_value = execute_result
            .try_as_basic_value()
            .left()
            .ok_or_else(|| anyhow!("Failed to get execution result"))?
            .into_int_value();

        // 检查执行结果
        let success_check = builder.build_int_compare(
            IntPredicate::EQ,
            result_value,
            i64_type.const_zero(),
            "execution_success",
        )?;
        builder.build_conditional_branch(success_check, cleanup_runtime, error_block)?;

        // Cleanup Runtime块：清理资源
        builder.position_at_end(cleanup_runtime);

        // 获取返回值（从栈顶）
        // 如果函数没有返回值，直接返回None
        let return_value = if function.get_type().get_return_type().is_some() {
            // 从VM栈顶获取返回值
            let stack_ptr = builder.build_struct_gep2(runtime_module.runtime_type, runtime_ptr, 0, "stack_ptr")?;

            let vm_value_temp = builder.build_alloca(self.codegen_helper.vm_value_type, "return_vm_value")?;

            let pop_result = builder.build_call(
                runtime_module.stack_module.functions.pop,
                &[stack_ptr.into(), vm_value_temp.into()],
                "pop_return_value",
            )?;

            // 检查pop是否成功
            let pop_success = builder.build_int_compare(
                IntPredicate::EQ,
                pop_result.try_as_basic_value().left().unwrap().into_int_value(),
                i32_type.const_zero(),
                "pop_success",
            )?;

            let convert_value_block = context.append_basic_block(
                builder.get_insert_block().unwrap().get_parent().unwrap(),
                "convert_value",
            );
            let default_value_block = context.append_basic_block(
                builder.get_insert_block().unwrap().get_parent().unwrap(),
                "default_value",
            );
            let phi_block = self
                .codegen_helper
                .context
                .append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "phi_return");

            builder.build_conditional_branch(pop_success, convert_value_block, default_value_block)?;

            // 转换VM值为LLVM值
            builder.position_at_end(convert_value_block);
            let converted_value_ref = self.convert_vm_value_to_llvm_value(&builder, vm_value_temp, function)?;
            builder.build_unconditional_branch(phi_block)?;

            // 默认值分支
            builder.position_at_end(default_value_block);
            let default_value_ref = self.create_default_return_value(&builder, function)?;
            builder.build_unconditional_branch(phi_block)?;

            // Phi节点合并值
            builder.position_at_end(phi_block);
            let phi = builder.build_phi(function.get_type().get_return_type().unwrap(), "return_phi")?;
            phi.add_incoming(&[
                (&converted_value_ref.into_basic_value_enum(), convert_value_block),
                (&default_value_ref.into_basic_value_enum(), default_value_block),
            ]);
            Some(phi.as_basic_value())
        } else {
            None
        };

        // 清理运行时
        builder.build_call(runtime_module.runtime_destroy, &[runtime_ptr.into()], "cleanup")?;

        builder.build_unconditional_branch(return_block)?;

        // Return块：返回结果
        builder.position_at_end(return_block);

        if function.get_type().get_return_type().is_some() {
            if let Some(ret_val) = return_value {
                builder.build_return(Some(&ret_val))?;
            } else {
                // 返回默认值
                let default_return = self.create_default_return_value(&builder, function)?;
                builder.build_return(Some(&default_return.into_basic_value_enum()))?;
            }
        } else {
            builder.build_return(None)?;
        }

        // Error块：错误处理
        builder.position_at_end(error_block);

        // 如果runtime_ptr不为空，需要清理
        let cleanup_check = builder.build_int_compare(
            IntPredicate::NE,
            runtime_ptr,
            runtime_module
                .runtime_type
                .ptr_type(AddressSpace::default())
                .const_null(),
            "need_cleanup",
        )?;

        let do_cleanup_block = context.append_basic_block(function, "do_cleanup");
        let error_return_block = context.append_basic_block(function, "error_return");

        builder.build_conditional_branch(cleanup_check, do_cleanup_block, error_return_block)?;

        builder.position_at_end(do_cleanup_block);
        builder.build_call(runtime_module.runtime_destroy, &[runtime_ptr.into()], "error_cleanup")?;
        builder.build_unconditional_branch(error_return_block)?;

        builder.position_at_end(error_return_block);
        if function.get_type().get_return_type().is_some() {
            builder.build_unreachable()?;
        } else {
            builder.build_return(None)?;
        }

        Ok(())
    }

    // 计算字节码长度
    fn calculate_bytecode_length(
        &self,
        builder: &Builder<'a>,
        instructions_data: &GlobalValue<'a>,
    ) -> Result<IntValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();

        // 获取全局常量的类型和大小
        let initializer = instructions_data
            .get_initializer()
            .ok_or_else(|| anyhow!("Instructions data has no initializer"))?;

        let array_type = initializer.get_type();

        // 获取数组长度
        let array_len = array_type.into_array_type().len();
        Ok(i64_type.const_int(array_len as u64, false))
    }

    // VM值转LLVM值
    fn convert_vm_value_to_llvm_value(
        &self,
        builder: &Builder<'a>,
        vm_value_ptr: PointerValue<'a>,
        function: FunctionValue,
    ) -> Result<LLVMValueRef> {
        let context = self.codegen_helper.context;
        let i8_type = context.i8_type();
        let i64_type = context.i64_type();

        let return_type = function
            .get_type()
            .get_return_type()
            .ok_or_else(|| anyhow!("Function has no return type"))?;

        let type_tag_ptr =
            builder.build_struct_gep2(self.codegen_helper.vm_value_type, vm_value_ptr, 0, "type_tag_ptr")?;
        let value_union_ptr =
            builder.build_struct_gep2(self.codegen_helper.vm_value_type, vm_value_ptr, 1, "value_union_ptr")?;

        let type_tag = builder.build_load2(i8_type, type_tag_ptr, "type_tag")?.into_int_value();
        let union_value = builder
            .build_load2(i64_type, value_union_ptr, "union_value")?
            .into_int_value();

        match return_type {
            BasicTypeEnum::IntType(int_type) => {
                let result: BasicValueEnum = match int_type.get_bit_width() {
                    1 => builder.build_int_truncate(union_value, int_type, "trunc_i1")?.into(),
                    8 => builder.build_int_truncate(union_value, int_type, "trunc_i8")?.into(),
                    16 => builder.build_int_truncate(union_value, int_type, "trunc_i16")?.into(),
                    32 => builder.build_int_truncate(union_value, int_type, "trunc_i32")?.into(),
                    64 => union_value.into(),
                    _ => return Err(anyhow!("Unsupported return integer bit width")),
                };
                Ok(result.as_value_ref() as LLVMValueRef)
            },
            BasicTypeEnum::FloatType(float_type) => {
                let result = builder.build_bit_cast(union_value, float_type, "union_as_float")?;
                Ok(result.as_value_ref() as LLVMValueRef)
            },
            BasicTypeEnum::PointerType(ptr_type) => {
                let result = builder.build_int_to_ptr(union_value, ptr_type, "union_as_ptr")?;
                Ok(result.as_value_ref() as LLVMValueRef)
            },
            _ => Err(anyhow!("Unsupported return type for VM conversion")),
        }
    }

    // 创建默认返回值
    fn create_default_return_value(&self, builder: &Builder<'a>, function: FunctionValue) -> Result<LLVMValueRef> {
        let return_type = function
            .get_type()
            .get_return_type()
            .ok_or_else(|| anyhow!("Function has no return type"))?;

        match return_type {
            BasicTypeEnum::IntType(int_type) => Ok(int_type.const_zero().as_value_ref() as LLVMValueRef),
            BasicTypeEnum::FloatType(float_type) => Ok(float_type.const_zero().as_value_ref() as LLVMValueRef),
            BasicTypeEnum::PointerType(ptr_type) => Ok(ptr_type.const_null().as_value_ref() as LLVMValueRef),
            _ => Err(anyhow!("Unsupported return type for default value")),
        }
    }
}

/// 栈模块构建器
struct StackModuleBuilder<'a, 'b> {
    codegen_helper: &'b CodeGenHelper<'a>,
}

impl<'a, 'b> StackModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b CodeGenHelper<'a>) -> Self {
        Self { codegen_helper: type_helper }
    }

    fn build(self, module: &mut Module<'a>) -> Result<VMStackModule<'a>> {
        let stack_type = self.create_stack_type();
        let functions = self.create_stack_functions(module, stack_type)?;

        Ok(VMStackModule { stack_type, functions })
    }

    fn create_stack_type(&self) -> StructType<'a> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());
        self.codegen_helper.context.struct_type(
            &[
                vm_value_ptr.into(), // data
                i64_type.into(),     // len
                i64_type.into(),     // cap
            ],
            false,
        )
    }

    fn create_stack_functions(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
    ) -> Result<StackFunctions<'a>> {
        let reserve = self.create_reserve_function(module, stack_type)?;
        Ok(StackFunctions {
            init: self.create_init_function(module, stack_type)?,
            destroy: self.create_destroy_function(module, stack_type)?,
            reserve,
            push: self.create_push_function(module, stack_type, reserve)?,
            pop: self.create_pop_function(module, stack_type)?,
            peek: self.create_peek_function(module, stack_type)?,
        })
    }

    fn create_init_function(&self, module: &mut Module<'a>, stack_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[stack_ptr.into()], false);

        let function = module.add_function("avm_init_stack", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();

        // 初始化所有字段为0/null
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());

        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;

        builder.build_store(data_gep, vm_value_ptr.const_null())?;
        builder.build_store(len_gep, i64_type.const_zero())?;
        builder.build_store(cap_gep, i64_type.const_zero())?;

        builder.build_return(None)?;
        Ok(function)
    }

    fn create_destroy_function(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());

        let fn_type = void_type.fn_type(&[stack_ptr.into()], false);

        let function = module.add_function("avm_stack_free", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();

        // 释放data指针，重置其他字段
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_ptr = builder
            .build_load2(vm_value_ptr, data_gep, "data")?
            .into_pointer_value();
        builder.build_free(data_ptr)?;

        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;

        builder.build_store(data_gep, vm_value_ptr.const_null())?;
        builder.build_store(len_gep, i64_type.const_zero())?;
        builder.build_store(cap_gep, i64_type.const_zero())?;

        builder.build_return(None)?;
        Ok(function)
    }

    fn create_reserve_function(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[stack_ptr.into(), i64_type.into()], false);

        let function = module.add_function("avm_stack_reserve", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();

        // reserve实现
        let entry = context.append_basic_block(function, "entry");
        let need_realloc = context.append_basic_block(function, "need_realloc");
        let do_realloc = context.append_basic_block(function, "do_realloc");
        let return_bb = context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_need = function.get_nth_param(1).unwrap().into_int_value();

        // 检查是否需要扩容
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;
        let cap_value = builder.build_load2(i64_type, cap_gep, "cap")?.into_int_value(); // 当前容量
        let need_more = builder.build_int_compare(IntPredicate::UGT, param_need, cap_value, "need_more")?; // 是否需要更多容量
        builder.build_conditional_branch(need_more, need_realloc, return_bb)?;

        builder.position_at_end(need_realloc);
        // 计算新容量：max(cap * 2, need, DEFAULT_STACK_CAPACITY)
        let new_cap = self.calculate_new_capacity(&builder, cap_value, param_need)?;
        builder.build_unconditional_branch(do_realloc)?;

        builder.position_at_end(do_realloc);
        self.reallocate_stack(&builder, stack_type, param_stack, new_cap)?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        builder.build_return(None)?;

        Ok(function)
    }

    fn calculate_new_capacity(
        &self,
        builder: &Builder<'a>,
        current_cap: IntValue<'a>,
        needed: IntValue<'a>,
    ) -> Result<IntValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();

        let doubled = builder.build_int_mul(current_cap, i64_type.const_int(2, false), "doubled")?;
        let default_cap = i64_type.const_int(DEFAULT_STACK_CAPACITY, false);

        // new_cap = max(doubled, needed, default_cap)
        let max1 = builder
            .build_select(
                builder.build_int_compare(IntPredicate::UGT, doubled, needed, "cmp1")?,
                doubled,
                needed,
                "max1",
            )?
            .into_int_value();

        let new_cap = builder
            .build_select(
                builder.build_int_compare(IntPredicate::UGT, max1, default_cap, "cmp2")?,
                max1,
                default_cap,
                "new_cap",
            )?
            .into_int_value();

        Ok(new_cap)
    }

    fn reallocate_stack(
        &self,
        builder: &Builder<'a>,
        stack_type: StructType<'a>,
        stack_ptr: PointerValue<'a>,
        new_cap: IntValue<'a>,
    ) -> Result<()> {
        let context = self.codegen_helper.context;
        let i8_type = context.i8_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());

        // 分配新内存
        let element_size = self.codegen_helper.vm_value_type.size_of().unwrap();
        let alloc_size = builder.build_int_mul(new_cap, element_size, "alloc_size")?;
        let new_data = builder.build_array_malloc(i8_type, alloc_size, "new_data")?;

        // 复制旧数据
        let data_gep = builder.build_struct_gep2(stack_type, stack_ptr, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, stack_ptr, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, stack_ptr, 2, "cap_ptr")?;

        let old_data = builder
            .build_load2(vm_value_ptr, data_gep, "old_data")?
            .into_pointer_value();
        let len_value = builder.build_load2(i64_type, len_gep, "len")?.into_int_value();

        // 如果有旧数据，复制并释放
        let has_data = builder.build_int_compare(IntPredicate::NE, old_data, vm_value_ptr.const_null(), "has_data")?;

        let copy_bb =
            context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "copy_data");
        let update_bb = context.append_basic_block(
            builder.get_insert_block().unwrap().get_parent().unwrap(),
            "update_pointers",
        );

        builder.build_conditional_branch(has_data, copy_bb, update_bb)?;

        builder.position_at_end(copy_bb);
        let copy_size = builder.build_int_mul(len_value, element_size, "copy_size")?;
        builder.build_memcpy(new_data, 8, old_data, 8, copy_size)?;
        builder.build_free(old_data)?;
        builder.build_unconditional_branch(update_bb)?;

        builder.position_at_end(update_bb);
        builder.build_store(data_gep, new_data)?;
        builder.build_store(cap_gep, new_cap)?;

        Ok(())
    }

    fn create_push_function(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
        reserve: FunctionValue,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i64_type = context.i64_type();
        let void_type = context.void_type();

        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[stack_ptr.into(), self.codegen_helper.vm_value_type.into()], false);
        let function = module.add_function("avm_stack_push", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_value = function.get_nth_param(1).unwrap().into_struct_value();

        // 创建一个容器保存传过来的值
        let tmp_value = builder.build_alloca(self.codegen_helper.vm_value_type, "")?;
        builder.build_store(tmp_value, param_value)?;

        // 读取长度，计算新长度
        let len_ptr = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_value = builder.build_load2(i64_type, len_ptr, "len")?.into_int_value();
        let new_len = builder.build_int_add(len_value, i64_type.const_int(1, false), "new_len")?;

        // 调用reserve确保容量
        builder.build_call(reserve, &[param_stack.into(), new_len.into()], "reserve_call")?;

        // 获取data的指针值(VMValue*)
        let data_ptr = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_value = builder
            .build_load2(
                self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default()),
                data_ptr,
                "data",
            )?
            .into_pointer_value();

        // data[len] = value
        let elem_ptr =
            builder.build_in_bounds_gep2(self.codegen_helper.vm_value_type, data_value, &[len_value], "elem_ptr")?;

        builder.build_memmove(
            elem_ptr,
            8,
            tmp_value,
            8,
            self.codegen_helper.vm_value_type.size_of().unwrap(),
        )?;

        builder.build_store(len_ptr, new_len)?;

        builder.build_return(None)?;

        Ok(function)
    }

    fn create_pop_function(&self, module: &mut Module<'a>, stack_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());
        let fn_type = i32_type.fn_type(&[stack_ptr.into(), vm_value_ptr.into()], false);
        let function = module.add_function("avm_stack_pop", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        let return_bb = context.append_basic_block(function, "return");
        let bb_if_len_eq_zero = context.append_basic_block(function, "if.then");
        let else_bb = context.append_basic_block(function, "if.else");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_out_value = function.get_nth_param(1).unwrap().into_pointer_value();

        // 检查栈是否为空
        let ret_val = builder.build_alloca(i32_type, "ret_val")?;
        let len_ptr = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_value = builder.build_load2(i64_type, len_ptr, "len")?.into_int_value();
        let is_empty = builder.build_int_compare(IntPredicate::EQ, len_value, i64_type.const_zero(), "is_empty")?;
        // 为空进入then分支，否则进入else分支
        builder.build_conditional_branch(is_empty, bb_if_len_eq_zero, else_bb)?;

        // if.then: 空栈，返回 -1
        builder.position_at_end(bb_if_len_eq_zero);
        let minus_one = i32_type.const_int_from_string("-1", StringRadix::Decimal).unwrap();
        builder.build_store(ret_val, minus_one)?;
        builder.build_unconditional_branch(return_bb)?;

        // if.else: 弹出栈顶
        builder.position_at_end(else_bb);
        let one_i64 = i64_type.const_int(1, false);
        let new_len = builder.build_int_sub(len_value, one_i64, "new_len")?;

        // 读取data指针
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_value = builder
            .build_load2(vm_value_ptr, data_gep, "data")?
            .into_pointer_value();

        // 取到元素地址 data[new_len]
        let elem_ptr =
            builder.build_in_bounds_gep2(self.codegen_helper.vm_value_type, data_value, &[new_len], "elem_ptr")?;

        // 拷贝到输出
        builder.build_memcpy(
            param_out_value,
            8,
            elem_ptr,
            8,
            self.codegen_helper.vm_value_type.size_of().unwrap(),
        )?;

        // 更新长度，并设置返回值为0
        builder.build_store(len_ptr, new_len)?;
        builder.build_store(ret_val, i32_type.const_zero())?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        let ret_val = builder.build_load2(i32_type, ret_val, "rt")?;
        builder.build_return(Some(&ret_val.into_int_value()))?;

        Ok(function)
    }

    fn create_peek_function(&self, module: &mut Module<'a>, stack_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default());
        let fn_type = i32_type.fn_type(&[stack_ptr.into(), vm_value_ptr.into()], false);
        let function = module.add_function("avm_stack_peek", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        // 实现peek逻辑：空栈返回 -1；否则复制栈顶元素并返回 0
        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        let then_bb = context.append_basic_block(function, "if.empty");
        let else_bb = context.append_basic_block(function, "if.non_empty");
        let return_bb = context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_out = function.get_nth_param(1).unwrap().into_pointer_value();

        let ret_val_ptr = builder.build_alloca(i32_type, "ret_val")?;

        // len
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_val = builder.build_load2(i64_type, len_gep, "len")?.into_int_value();

        let is_empty = builder.build_int_compare(IntPredicate::EQ, len_val, i64_type.const_zero(), "is_empty")?;
        builder.build_conditional_branch(is_empty, then_bb, else_bb)?;

        // 空栈分支：写 -1
        builder.position_at_end(then_bb);
        let minus_one = i32_type.const_int_from_string("-1", StringRadix::Decimal).unwrap();
        builder.build_store(ret_val_ptr, minus_one)?;
        builder.build_unconditional_branch(return_bb)?;

        // 非空分支：复制栈顶，不修改len
        builder.position_at_end(else_bb);
        let one_i64 = i64_type.const_int(1, false);
        let top_index = builder.build_int_sub(len_val, one_i64, "top_index")?;

        // data[top_index]
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_ptr = builder
            .build_load2(
                self.codegen_helper.vm_value_type.ptr_type(AddressSpace::default()),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let elem_ptr =
            builder.build_in_bounds_gep2(self.codegen_helper.vm_value_type, data_ptr, &[top_index], "elem_ptr")?;

        builder.build_memcpy(
            param_out,
            8,
            elem_ptr,
            8,
            self.codegen_helper.vm_value_type.size_of().unwrap(),
        )?;
        builder.build_store(ret_val_ptr, i32_type.const_zero())?;
        builder.build_unconditional_branch(return_bb)?;

        // return
        builder.position_at_end(return_bb);
        let ret_val = builder.build_load2(i32_type, ret_val_ptr, "ret")?;
        builder.build_return(Some(&ret_val.into_int_value()))?;

        Ok(function)
    }
}

/// 寄存器模块构建器
struct RegisterModuleBuilder<'a, 'b> {
    codegen_helper: &'b CodeGenHelper<'a>,
    max_registers: u32,
}

impl<'a, 'b> RegisterModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b CodeGenHelper<'a>, max_registers: u32) -> Self {
        Self {
            codegen_helper: type_helper,
            max_registers,
        }
    }

    fn build(self, module: &mut Module<'a>) -> Result<VMRegisterModule<'a>> {
        let array_type = self.codegen_helper.vm_value_type.array_type(self.max_registers + 1);
        let set_value = self.create_set_value_function(module, array_type)?;
        let get_value = self.create_get_value_function(module, array_type)?;

        Ok(VMRegisterModule { array_type, set_value, get_value })
    }

    fn create_set_value_function(
        &self,
        module: &mut Module<'a>,
        array_type: ArrayType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let array_ptr = array_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(
            &[array_ptr.into(), i32_type.into(), self.codegen_helper.vm_value_type.into()],
            false,
        );

        let function = module.add_function("avm_set_register_value", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        // 实现寄存器写入逻辑
        let builder = context.create_builder();

        // 参数
        let param_array = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_index = function.get_nth_param(1).unwrap().into_int_value();
        let param_value = function.get_nth_param(2).unwrap().into_struct_value();

        // 基本块
        let entry = context.append_basic_block(function, "entry");
        let oob_bb = context.append_basic_block(function, "oob"); // 越界
        let in_bounds = context.append_basic_block(function, "in_bounds"); // 合法

        builder.position_at_end(entry);

        // 检测越界：index >= array_len
        let array_len = i32_type.const_int(array_type.len() as u64, false);
        let cond = builder.build_int_compare(IntPredicate::UGE, param_index, array_len, "index_oob")?;
        builder.build_conditional_branch(cond, oob_bb, in_bounds)?;

        // 越界分支：unreachable
        builder.position_at_end(oob_bb);
        builder.build_unreachable()?;

        // 写入分支
        builder.position_at_end(in_bounds);
        let i32_zero = i32_type.const_zero();
        let elem_ptr = builder.build_in_bounds_gep2(array_type, param_array, &[i32_zero, param_index], "elem_ptr")?;
        builder.build_store(elem_ptr, param_value)?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_get_value_function( &self,
                                  module: &mut Module<'a>,
                                  array_type: ArrayType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.codegen_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let array_ptr = array_type.ptr_type(AddressSpace::default());
        let vm_value_type = self.codegen_helper.vm_value_type;
        let vm_value_ptr = vm_value_type.ptr_type(AddressSpace::default());

        let fn_type = i32_type.fn_type(
            &[array_ptr.into(), i32_type.into(), vm_value_ptr.into()],
            false,
        );

        let i32_zero = i32_type.const_zero();
        let i32_minus_one = i32_type.const_int_from_string("-1", StringRadix::Decimal).unwrap();

        let function = module.add_function("avm_set_register_value", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.codegen_helper.inline_attr);

        // 实现寄存器读取逻辑
        let builder = context.create_builder();

        // 参数
        let param_array = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_index = function.get_nth_param(1).unwrap().into_int_value();
        let param_out_value = function.get_nth_param(2).unwrap().into_pointer_value();

        // 基本块
        let entry = context.append_basic_block(function, "entry");
        let oob_bb = context.append_basic_block(function, "oob"); // 越界
        let in_bounds = context.append_basic_block(function, "in_bounds"); // 合法

        builder.position_at_end(entry);

        // 检测越界：index >= array_len
        let array_len = i32_type.const_int(array_type.len() as u64, false);
        let cond = builder.build_int_compare(IntPredicate::UGE, param_index, array_len, "index_oob")?;
        builder.build_conditional_branch(cond, oob_bb, in_bounds)?;

        // 越界分支：unreachable
        builder.position_at_end(oob_bb);
        builder.build_return(Some(&i32_minus_one))?;

        // 写入分支
        builder.position_at_end(in_bounds);
        let elem_ptr = builder.build_in_bounds_gep2(array_type, param_array, &[i32_zero, param_index], "elem_ptr")?;
        let value = builder.build_load2(vm_value_type, elem_ptr, "value")?;
        builder.build_store(param_out_value, value)?;
        builder.build_return(Some(&i32_zero))?;

        Ok(function)
    }
}

/// 内存模块构建器
struct MemoryModuleBuilder<'a, 'b> {
    type_helper: &'b CodeGenHelper<'a>,
}

impl<'a, 'b> MemoryModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b CodeGenHelper<'a>) -> Self {
        Self { type_helper }
    }

    fn build(
        self,
        module: &mut Module<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
        get_value_size_by_tag: FunctionValue,
    ) -> Result<VMMemoryModule<'a>> {
        let memory_type = self.create_memory_type();
        let functions = self.create_memory_functions(module, memory_type, value_type_map, get_value_size_by_tag)?;

        Ok(VMMemoryModule { memory_type, functions })
    }

    fn create_memory_type(&self) -> StructType<'a> {
        let context = self.type_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);

        self.type_helper.context.struct_type(
            &[
                i8_ptr.into(),   // data
                i64_type.into(), // size
                i64_type.into(), // next_addr
            ],
            false,
        )
    }

    fn create_memory_functions(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
        get_value_size_by_tag: FunctionValue,
    ) -> Result<MemoryFunctions<'a>> {
        let ensure_fn = self.create_ensure_function(module, memory_type)?;
        Ok(MemoryFunctions {
            init: self.create_init_function(module, memory_type)?,
            destroy: self.create_destroy_function(module, memory_type)?,
            ensure: ensure_fn,
            alloc: self.create_alloc_function(module, memory_type)?,
            store_value: self.create_store_value_function(
                module,
                memory_type,
                value_type_map,
                ensure_fn,
                get_value_size_by_tag,
            )?,
            load_value: self.create_load_value_function(module, memory_type, value_type_map, get_value_size_by_tag)?,
        })
    }

    fn create_init_function(&self, module: &mut Module<'a>, memory_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();

        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[memory_ptr.into()], false);
        let function = module.add_function("avm_mem_init", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();

        // 初始化内存结构
        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;

        let data_ptr =
            builder.build_array_malloc(i8_type, i32_type.const_int(AVM_MEM_DEFAULT_SIZE, false), "data_ptr")?;

        builder.build_store(data_gep, data_ptr)?;
        builder.build_store(size_gep, i64_type.const_int(AVM_MEM_DEFAULT_SIZE, false))?;
        builder.build_store(next_addr_gep, i64_type.const_int(0x1000, false))?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_destroy_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let context = self.type_helper.context;
        let i8_ptr = ptr_type!(context, i8_type);

        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[memory_ptr.into()], false);

        let function = module.add_function("avm_mem_free", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();

        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let data_value = builder.build_load2(i8_ptr, data_gep, "data")?.into_pointer_value();
        builder.build_free(data_value)?;

        // 重置所有字段
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;

        builder.build_store(size_gep, i64_type.const_zero())?;
        builder.build_store(next_addr_gep, i64_type.const_zero())?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_ensure_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[memory_ptr.into(), i64_type.into()], false);

        let function = module.add_function("avm_mem_ensure", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = context.create_builder();

        // ensure实现
        let entry = context.append_basic_block(function, "entry");
        let check_size = context.append_basic_block(function, "check_size");
        let expand_memory = context.append_basic_block(function, "expand_memory");
        let return_bb = context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_need = function.get_nth_param(1).unwrap().into_int_value();

        builder.build_unconditional_branch(check_size)?;

        builder.position_at_end(check_size);
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let size_value = builder.build_load2(i64_type, size_gep, "size")?.into_int_value();

        // 判断是否需要扩容
        let need_expand = builder.build_int_compare(IntPredicate::UGT, param_need, size_value, "need_expand")?;
        builder.build_conditional_branch(need_expand, expand_memory, return_bb)?;

        builder.position_at_end(expand_memory);
        // 计算新大小：以页面大小对齐的扩容
        let new_size = self.calculate_new_memory_size(&builder, size_value, param_need)?;
        self.reallocate_memory(&builder, memory_type, param_memory, new_size)?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        builder.build_return(None)?;

        Ok(function)
    }

    fn calculate_new_memory_size(
        &self,
        builder: &Builder<'a>,
        current_size: IntValue<'a>,
        needed: IntValue<'a>,
    ) -> Result<IntValue<'a>> {
        let context = self.type_helper.context;
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();

        // new_size = current_size
        // while (new_size < needed) new_size += new_size/2 + PAGE_SIZE
        let new_size_ptr = builder.build_alloca(i64_type, "new_size")?;
        builder.build_store(new_size_ptr, current_size)?;

        let loop_cond =
            context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_cond");
        let loop_body =
            context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_body");
        let loop_end =
            context.append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_end");

        builder.build_unconditional_branch(loop_cond)?;

        builder.position_at_end(loop_cond);
        let new_size_val = builder
            .build_load2(i64_type, new_size_ptr, "new_size_val")?
            .into_int_value();
        let need_more = builder.build_int_compare(IntPredicate::ULT, new_size_val, needed, "need_more")?;
        builder.build_conditional_branch(need_more, loop_body, loop_end)?;

        builder.position_at_end(loop_body);
        let half = builder.build_int_unsigned_div(new_size_val, i64_type.const_int(2, false), "half")?;
        let increment = builder.build_int_add(half, i64_type.const_int(AVM_PAGE_SIZE, false), "increment")?;
        let updated_size = builder.build_int_add(new_size_val, increment, "updated_size")?;
        builder.build_store(new_size_ptr, updated_size)?;
        builder.build_unconditional_branch(loop_cond)?;

        builder.position_at_end(loop_end);
        let final_size = builder
            .build_load2(i64_type, new_size_ptr, "final_size")?
            .into_int_value();
        Ok(final_size)
    }

    fn reallocate_memory(
        &self,
        builder: &Builder<'a>,
        memory_type: StructType<'a>,
        memory_ptr: PointerValue<'a>,
        new_size: IntValue<'a>,
    ) -> Result<()> {
        let context = self.type_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);

        // 分配新内存并复制旧数据
        let data_gep = builder.build_struct_gep2(memory_type, memory_ptr, 0, "data_ptr")?;
        let size_gep = builder.build_struct_gep2(memory_type, memory_ptr, 1, "size_ptr")?;

        let old_data = builder.build_load2(i8_ptr, data_gep, "old_data")?.into_pointer_value();
        let old_size = builder.build_load2(i64_type, size_gep, "old_size")?.into_int_value();

        let new_data = builder.build_array_malloc(i8_type, new_size, "new_data")?;
        builder.build_memcpy(new_data, 8, old_data, 8, old_size)?;
        builder.build_free(old_data)?;

        builder.build_store(data_gep, new_data)?;
        builder.build_store(size_gep, new_size)?;

        Ok(())
    }

    fn create_alloc_function(&self, module: &mut Module<'a>, memory_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = i64_type.fn_type(&[memory_ptr.into(), i64_type.into()], false);
        let function = module.add_function("avm_mem_alloc", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_size = function.get_nth_param(1).unwrap().into_int_value();

        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;
        let next_addr = builder
            .build_load2(i64_type, next_addr_gep, "next_addr")?
            .into_int_value();

        let new_next_addr = builder.build_int_add(next_addr, param_size, "new_next_addr")?;
        let need_with_slack = builder.build_int_add(
            new_next_addr,
            i64_type.const_int(AVM_ALLOC_SLACK, false),
            "need_with_slack",
        )?;

        // 调用ensure确保内存足够
        let ensure_fn = module.get_function("avm_mem_ensure").unwrap();
        builder.build_call(ensure_fn, &[param_memory.into(), need_with_slack.into()], "")?;

        builder.build_store(next_addr_gep, new_next_addr)?;
        builder.build_return(Some(&next_addr))?;

        Ok(function)
    }

    fn create_store_value_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
        ensure_fn: FunctionValue,
        get_value_size_by_tag: FunctionValue,
    ) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        let fn_type = void_type.fn_type(&[memory_ptr.into(), i64_type.into(), vm_value_ptr.into()], false);
        let function = module.add_function("avm_mem_store_value", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 使用简化的store_value实现
        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");
        let store_tag = context.append_basic_block(function, "store_tag");
        let store_value = context.append_basic_block(function, "store_value");
        let return_bb = context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_addr = function.get_nth_param(1).unwrap().into_int_value();
        let param_value = function.get_nth_param(2).unwrap().into_pointer_value();

        // 读取类型标签
        let tag_gep = builder.build_struct_gep2(self.type_helper.vm_value_type, param_value, 0, "tag_ptr")?;
        let tag_value = builder.build_load2(i8_type, tag_gep, "tag")?.into_int_value();

        // 计算值大小
        let value_size = builder
            .build_call(get_value_size_by_tag, &[tag_value.into()], "value_size_call")?
            .try_as_basic_value()
            .left()
            .unwrap()
            .into_int_value();

        // 确保内存足够
        let total_need = builder.build_int_add(
            param_addr,
            builder.build_int_z_extend(value_size, i64_type, "size_i64")?,
            "need",
        )?;
        let need_plus_tag = builder.build_int_add(total_need, i64_type.const_int(1, false), "need_plus_tag")?;

        builder.build_call(ensure_fn, &[param_memory.into(), need_plus_tag.into()], "")?;

        builder.build_unconditional_branch(store_tag)?;

        builder.position_at_end(store_tag);
        // 存储类型标签在addr-1位置
        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let data_ptr = builder.build_load2(i8_ptr, data_gep, "data")?.into_pointer_value();

        let tag_offset = builder.build_int_sub(param_addr, i64_type.const_int(1, false), "tag_offset")?;
        let tag_dest = builder.build_in_bounds_gep2(i8_type, data_ptr, &[tag_offset], "tag_dest")?;

        // 把标签类型填入内存
        let tag_byte = builder.build_int_truncate(tag_value, i8_type, "tag_byte")?;
        builder.build_store(tag_dest, tag_byte)?;

        // 检查是否需要存储值数据
        let is_zero_size =
            builder.build_int_compare(IntPredicate::EQ, value_size, i32_type.const_zero(), "is_zero_size")?;
        builder.build_conditional_branch(is_zero_size, return_bb, store_value)?;

        builder.position_at_end(store_value);
        // 存储值数据
        let value_gep = builder.build_struct_gep2(self.type_helper.vm_value_type, param_value, 1, "value_ptr")?;
        let value_dest = builder.build_in_bounds_gep2(i8_type, data_ptr, &[param_addr], "value_dest")?;

        builder.build_memcpy(value_dest, 8, value_gep, 8, value_size)?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_load_value_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
        get_value_size_by_tag: FunctionValue,
    ) -> Result<FunctionValue<'a>> {
        let context = self.type_helper.context;
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let i8_ptr = ptr_type!(context, i8_type);
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        let fn_type = void_type.fn_type(
            &[
                memory_ptr.into(),
                i64_type.into(),     // addr
                vm_value_ptr.into(), // out_value
            ],
            false,
        );
        let function = module.add_function("avm_mem_load_value", fn_type, DEFAULT_LINKAGE);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 使用简化的store_value实现
        let builder = context.create_builder();
        let entry = context.append_basic_block(function, "entry");

        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_addr = function.get_nth_param(1).unwrap().into_int_value();
        let param_output = function.get_nth_param(2).unwrap().into_pointer_value();

        // 清空，方便接下来往里面写数据
        builder.build_memset(
            param_output,
            8,
            i8_type.const_zero(),
            self.type_helper.vm_value_type.size_of().unwrap(),
        )?;

        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let data_ptr = builder.build_load2(i8_ptr, data_gep, "data")?.into_pointer_value();

        let tag_offset = builder.build_int_sub(param_addr, i64_type.const_int(1, false), "tag_offset")?;
        // 从内存获取标签类型，从而获取拷贝长度
        let tag_ptr = builder.build_in_bounds_gep2(i8_type, data_ptr, &[tag_offset], "tag_ptr")?;
        let type_tag = builder.build_load2(i8_type, tag_ptr, "type_tag")?.into_int_value();

        let output_tag_ptr =
            builder.build_struct_gep2(self.type_helper.vm_value_type, param_output, 0, "output_tag_ptr")?;
        builder.build_store(output_tag_ptr, type_tag)?;

        // 根据类型读取值数据
        let value_ptr = builder.build_in_bounds_gep2(i8_type, data_ptr, &[param_addr], "value_ptr")?;
        let output_value_ptr =
            builder.build_struct_gep2(self.type_helper.vm_value_type, param_output, 1, "output_value_ptr")?;

        let value_size = builder
            .build_call(get_value_size_by_tag, &[type_tag.into()], "value_size_call")?
            .try_as_basic_value()
            .left()
            .unwrap()
            .into_int_value();
        builder.build_memcpy(output_value_ptr, 8, value_ptr, 8, value_size)?;

        builder.build_return(None)?;
        Ok(function)
    }
}

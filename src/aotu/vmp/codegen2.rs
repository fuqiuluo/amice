use crate::aotu::vmp::bytecode::{BytecodeValueType, VMPBytecodeEncoder};
use crate::aotu::vmp::compiler::VMPCompilerContext;
use amice_llvm::inkwell2::BuilderExt;
use anyhow::{Result, anyhow};
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::{ArrayType, BasicType, IntType, PointerType, StringRadix, StructType, VoidType};
use llvm_plugin::inkwell::values::{FunctionValue, GlobalValue, IntValue, PointerValue};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use std::collections::HashMap;

const AVM_MEM_DEFAULT_SIZE: u64 = 1024 * 1024;
const AVM_PAGE_SIZE: u64 = 4096;
const AVM_ALLOC_SLACK: u64 = 1024;
const DEFAULT_STACK_CAPACITY: u64 = 16;

/// VM代码生成器主类
pub struct VMPCodeGenerator<'a, 'b> {
    module: &'b mut Module<'a>,
    encoder: VMPBytecodeEncoder,
    type_helper: TypeHelper<'a>,
}

/// 类型和常量的统一管理
struct TypeHelper<'a> {
    context: ContextRef<'a>,
    vm_value_type: StructType<'a>,
    inline_attr: Attribute,
}

impl<'a> TypeHelper<'a> {
    fn new(context: ContextRef<'a>) -> Self {
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

    fn i8_type(&self) -> IntType<'a> {
        self.context.i8_type()
    }
    fn i32_type(&self) -> IntType<'a> {
        self.context.i32_type()
    }
    fn i64_type(&self) -> IntType<'a> {
        self.context.i64_type()
    }
    fn void_type(&self) -> VoidType<'a> {
        self.context.void_type()
    }
    fn ptr_type(&self) -> PointerType<'a> {
        self.context.i8_type().ptr_type(AddressSpace::default())
    }
}

/// 运行时函数集合
struct RuntimeFunctions<'a> {
    avm_runtime_new: FunctionValue<'a>,
    avm_runtime_destroy: FunctionValue<'a>,
    avm_runtime_execute: FunctionValue<'a>,
}

/// VM栈模块
struct VMStackModule<'a> {
    stack_type: StructType<'a>,
    functions: StackFunctions<'a>,
}

struct StackFunctions<'a> {
    init: FunctionValue<'a>,
    destroy: FunctionValue<'a>,
    reserve: FunctionValue<'a>,
    push: FunctionValue<'a>,
    pop: FunctionValue<'a>,
    peek: FunctionValue<'a>,
}

/// VM寄存器模块
struct VMRegisterModule<'a> {
    array_type: ArrayType<'a>,
    set_value: FunctionValue<'a>,
}

/// VM内存模块
struct VMMemoryModule<'a> {
    memory_type: StructType<'a>,
    functions: MemoryFunctions<'a>,
}

struct MemoryFunctions<'a> {
    init: FunctionValue<'a>,
    destroy: FunctionValue<'a>,
    ensure: FunctionValue<'a>,
    alloc: FunctionValue<'a>,
    store_value: FunctionValue<'a>,
}

impl<'a, 'b> VMPCodeGenerator<'a, 'b> {
    pub fn new(module: &'b mut Module<'a>) -> Result<Self> {
        let context = module.get_context();
        let type_helper = TypeHelper::new(context);

        Ok(Self {
            module,
            encoder: VMPBytecodeEncoder::new(),
            type_helper,
        })
    }

    /// 编译函数为VM调用
    pub fn compile_function_to_vm_call(&mut self, function: FunctionValue, context: VMPCompilerContext) -> Result<()> {
        let instructions_data = self.serialize_instructions(&context)?;
        let runtime_functions = self.create_runtime_functions(context)?;
        self.replace_function_body(function, instructions_data, runtime_functions)?;
        Ok(())
    }

    /// 序列化指令到全局常量
    fn serialize_instructions(&mut self, compiler_context: &VMPCompilerContext) -> Result<GlobalValue<'a>> {
        let bytecode_data = self
            .encoder
            .encode_instructions(compiler_context.finalize())
            .map_err(|e| anyhow!("Failed to serialize instructions: {}", e))?;

        let global_name = format!("__avm_bytecode_{}", rand::random::<u32>());
        let array_type = self.type_helper.i8_type().array_type(bytecode_data.len() as u32);

        let global_value = self
            .module
            .add_global(array_type, Some(AddressSpace::default()), &global_name);

        let byte_values: Vec<_> = bytecode_data
            .iter()
            .map(|&b| self.type_helper.i8_type().const_int(b as u64, false))
            .collect();
        let array_value = self.type_helper.i8_type().const_array(&byte_values);

        global_value.set_initializer(&array_value);
        global_value.set_constant(true);
        global_value.set_linkage(Linkage::Private);

        Ok(global_value)
    }

    /// 创建所有运行时函数
    fn create_runtime_functions(&mut self, vmp_context: VMPCompilerContext) -> Result<RuntimeFunctions<'a>> {
        let stack_module = StackModuleBuilder::new(&self.type_helper).build(self.module)?;
        let register_module =
            RegisterModuleBuilder::new(&self.type_helper, vmp_context.get_max_register()).build(self.module)?;
        let memory_module =
            MemoryModuleBuilder::new(&self.type_helper).build(self.module, self.encoder.get_value_type_map())?;

        // 创建主要的运行时函数
        let avm_runtime_new = self.create_runtime_new()?;
        let avm_runtime_destroy = self.create_runtime_destroy()?;
        let avm_runtime_execute = self.create_runtime_execute()?;

        Ok(RuntimeFunctions {
            avm_runtime_new,
            avm_runtime_destroy,
            avm_runtime_execute,
        })
    }

    fn create_runtime_new(&self) -> Result<FunctionValue<'a>> {
        let fn_type = self.type_helper.ptr_type().fn_type(&[], false);
        let function = self.module.add_function("avm_runtime_new", fn_type, None);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);
        Ok(function)
    }

    fn create_runtime_destroy(&self) -> Result<FunctionValue<'a>> {
        let fn_type = self
            .type_helper
            .void_type()
            .fn_type(&[self.type_helper.ptr_type().into()], false);
        let function = self.module.add_function("avm_runtime_destroy", fn_type, None);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);
        Ok(function)
    }

    fn create_runtime_execute(&self) -> Result<FunctionValue<'a>> {
        let fn_type = self.type_helper.i64_type().fn_type(
            &[
                self.type_helper.ptr_type().into(),
                self.type_helper.ptr_type().into(),
                self.type_helper.i64_type().into(),
            ],
            false,
        );
        let function = self.module.add_function("avm_runtime_execute", fn_type, None);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 创建空的函数体
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let builder = self.type_helper.context.create_builder();
        builder.position_at_end(entry);
        builder.build_return(None)?;

        Ok(function)
    }

    fn replace_function_body(
        &self,
        _function: FunctionValue,
        _instructions_data: GlobalValue<'a>,
        _runtime_functions: RuntimeFunctions,
    ) -> Result<()> {
        // TODO: 实现函数体替换逻辑
        Ok(())
    }
}

/// 栈模块构建器
struct StackModuleBuilder<'a, 'b> {
    type_helper: &'b TypeHelper<'a>,
}

impl<'a, 'b> StackModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b TypeHelper<'a>) -> Self {
        Self { type_helper }
    }

    fn build(self, module: &mut Module<'a>) -> Result<VMStackModule<'a>> {
        let stack_type = self.create_stack_type();
        let functions = self.create_stack_functions(module, stack_type)?;

        Ok(VMStackModule { stack_type, functions })
    }

    fn create_stack_type(&self) -> StructType<'a> {
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());
        self.type_helper.context.struct_type(
            &[
                vm_value_ptr.into(),                // data
                self.type_helper.i64_type().into(), // len
                self.type_helper.i64_type().into(), // cap
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
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = self.type_helper.void_type().fn_type(&[stack_ptr.into()], false);
        let function = module.add_function("avm_init_stack", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();

        // 初始化所有字段为0/null
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;

        builder.build_store(data_gep, vm_value_ptr.const_null())?;
        builder.build_store(len_gep, self.type_helper.i64_type().const_zero())?;
        builder.build_store(cap_gep, self.type_helper.i64_type().const_zero())?;

        builder.build_return(None)?;
        Ok(function)
    }

    fn create_destroy_function(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = self.type_helper.void_type().fn_type(&[stack_ptr.into()], false);
        let function = module.add_function("avm_stack_free", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();

        // 释放data指针，重置其他字段
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());
        let data_ptr = builder
            .build_load2(vm_value_ptr, data_gep, "data")?
            .into_pointer_value();
        builder.build_free(data_ptr)?;

        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;

        builder.build_store(data_gep, vm_value_ptr.const_null())?;
        builder.build_store(len_gep, self.type_helper.i64_type().const_zero())?;
        builder.build_store(cap_gep, self.type_helper.i64_type().const_zero())?;

        builder.build_return(None)?;
        Ok(function)
    }

    fn create_reserve_function(
        &self,
        module: &mut Module<'a>,
        stack_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .void_type()
            .fn_type(&[stack_ptr.into(), self.type_helper.i64_type().into()], false);
        let function = module.add_function("avm_stack_reserve", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();

        // reserve实现
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let need_realloc = self.type_helper.context.append_basic_block(function, "need_realloc");
        let do_realloc = self.type_helper.context.append_basic_block(function, "do_realloc");
        let return_bb = self.type_helper.context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_need = function.get_nth_param(1).unwrap().into_int_value();

        // 检查是否需要扩容
        let cap_gep = builder.build_struct_gep2(stack_type, param_stack, 2, "cap_ptr")?;
        let cap_value = builder
            .build_load2(self.type_helper.i64_type(), cap_gep, "cap")?
            .into_int_value(); // 当前容量
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
        let doubled = builder.build_int_mul(current_cap, self.type_helper.i64_type().const_int(2, false), "doubled")?;
        let default_cap = self.type_helper.i64_type().const_int(DEFAULT_STACK_CAPACITY, false);

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
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        // 分配新内存
        let element_size = self.type_helper.vm_value_type.size_of().unwrap();
        let alloc_size = builder.build_int_mul(new_cap, element_size, "alloc_size")?;
        let new_data = builder.build_array_malloc(self.type_helper.i8_type(), alloc_size, "new_data")?;

        // 复制旧数据
        let data_gep = builder.build_struct_gep2(stack_type, stack_ptr, 0, "data_ptr")?;
        let len_gep = builder.build_struct_gep2(stack_type, stack_ptr, 1, "len_ptr")?;
        let cap_gep = builder.build_struct_gep2(stack_type, stack_ptr, 2, "cap_ptr")?;

        let old_data = builder
            .build_load2(vm_value_ptr, data_gep, "old_data")?
            .into_pointer_value();
        let len_value = builder
            .build_load2(self.type_helper.i64_type(), len_gep, "len")?
            .into_int_value();

        // 如果有旧数据，复制并释放
        let has_data = builder.build_int_compare(IntPredicate::NE, old_data, vm_value_ptr.const_null(), "has_data")?;

        let copy_bb = self
            .type_helper
            .context
            .append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "copy_data");
        let update_bb = self.type_helper.context.append_basic_block(
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
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .void_type()
            .fn_type(&[stack_ptr.into(), self.type_helper.vm_value_type.into()], false);
        let function = module.add_function("avm_stack_push", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_value = function.get_nth_param(1).unwrap().into_struct_value();

        // 创建一个容器保存传过来的值
        let tmp_value = builder.build_alloca(self.type_helper.vm_value_type, "")?;
        builder.build_store(tmp_value, param_value)?;

        // 读取长度，计算新长度
        let len_ptr = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_value = builder
            .build_load2(self.type_helper.i64_type(), len_ptr, "len")?
            .into_int_value();
        let new_len = builder.build_int_add(len_value, self.type_helper.i64_type().const_int(1, false), "new_len")?;

        // 调用reserve确保容量
        builder.build_call(reserve, &[param_stack.into(), new_len.into()], "reserve_call")?;

        // 获取data的指针值(VMValue*)
        let data_ptr = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_value = builder
            .build_load2(
                self.type_helper.vm_value_type.ptr_type(AddressSpace::default()),
                data_ptr,
                "data",
            )?
            .into_pointer_value();

        // data[len] = value
        let elem_ptr =
            builder.build_in_bounds_gep2(self.type_helper.vm_value_type, data_value, &[len_value], "elem_ptr")?;

        builder.build_memmove(
            elem_ptr,
            8,
            tmp_value,
            8,
            self.type_helper.vm_value_type.size_of().unwrap(),
        )?;

        builder.build_store(len_ptr, new_len)?;

        builder.build_return(None)?;

        Ok(function)
    }

    fn create_pop_function(&self, module: &mut Module<'a>, stack_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .i32_type()
            .fn_type(&[stack_ptr.into(), vm_value_ptr.into()], false);
        let function = module.add_function("avm_stack_pop", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let return_bb = self.type_helper.context.append_basic_block(function, "return");
        let bb_if_len_eq_zero = self.type_helper.context.append_basic_block(function, "if.then");
        let else_bb = self.type_helper.context.append_basic_block(function, "if.else");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_out_value = function.get_nth_param(1).unwrap().into_pointer_value();

        // 检查栈是否为空
        let ret_val = builder.build_alloca(self.type_helper.i32_type(), "ret_val")?;
        let len_ptr = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_value = builder
            .build_load2(self.type_helper.i64_type(), len_ptr, "len")?
            .into_int_value();
        let is_empty = builder.build_int_compare(
            IntPredicate::EQ,
            len_value,
            self.type_helper.i64_type().const_zero(),
            "is_empty",
        )?;
        // 为空进入then分支，否则进入else分支
        builder.build_conditional_branch(is_empty, bb_if_len_eq_zero, else_bb)?;

        // if.then: 空栈，返回 -1
        builder.position_at_end(bb_if_len_eq_zero);
        let minus_one = self
            .type_helper
            .i32_type()
            .const_int_from_string("-1", StringRadix::Decimal)
            .unwrap();
        builder.build_store(ret_val, minus_one)?;
        builder.build_unconditional_branch(return_bb)?;

        // if.else: 弹出栈顶
        builder.position_at_end(else_bb);
        let one_i64 = self.type_helper.i64_type().const_int(1, false);
        let new_len = builder.build_int_sub(len_value, one_i64, "new_len")?;

        // 读取data指针
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_value = builder
            .build_load2(
                self.type_helper.vm_value_type.ptr_type(AddressSpace::default()),
                data_gep,
                "data",
            )?
            .into_pointer_value();

        // 取到元素地址 data[new_len]
        let elem_ptr =
            builder.build_in_bounds_gep2(self.type_helper.vm_value_type, data_value, &[new_len], "elem_ptr")?;

        // 拷贝到输出
        builder.build_memcpy(
            param_out_value,
            8,
            elem_ptr,
            8,
            self.type_helper.vm_value_type.size_of().unwrap(),
        )?;

        // 更新长度，并设置返回值为0
        builder.build_store(len_ptr, new_len)?;
        builder.build_store(ret_val, self.type_helper.i32_type().const_zero())?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        let ret_val = builder.build_load2(self.type_helper.i32_type(), ret_val, "rt")?;
        builder.build_return(Some(&ret_val.into_int_value()))?;

        Ok(function)
    }

    fn create_peek_function(&self, module: &mut Module<'a>, stack_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let stack_ptr = stack_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .i32_type()
            .fn_type(&[stack_ptr.into(), vm_value_ptr.into()], false);
        let function = module.add_function("avm_stack_peek", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 实现peek逻辑：空栈返回 -1；否则复制栈顶元素并返回 0
        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let then_bb = self.type_helper.context.append_basic_block(function, "if.empty");
        let else_bb = self.type_helper.context.append_basic_block(function, "if.non_empty");
        let return_bb = self.type_helper.context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_stack = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_out = function.get_nth_param(1).unwrap().into_pointer_value();

        let ret_val_ptr = builder.build_alloca(self.type_helper.i32_type(), "ret_val")?;

        // len
        let len_gep = builder.build_struct_gep2(stack_type, param_stack, 1, "len_ptr")?;
        let len_val = builder
            .build_load2(self.type_helper.i64_type(), len_gep, "len")?
            .into_int_value();

        let is_empty = builder.build_int_compare(
            IntPredicate::EQ,
            len_val,
            self.type_helper.i64_type().const_zero(),
            "is_empty",
        )?;
        builder.build_conditional_branch(is_empty, then_bb, else_bb)?;

        // 空栈分支：写 -1
        builder.position_at_end(then_bb);
        let minus_one = self
            .type_helper
            .i32_type()
            .const_int_from_string("-1", StringRadix::Decimal)
            .unwrap();
        builder.build_store(ret_val_ptr, minus_one)?;
        builder.build_unconditional_branch(return_bb)?;

        // 非空分支：复制栈顶，不修改len
        builder.position_at_end(else_bb);
        let one_i64 = self.type_helper.i64_type().const_int(1, false);
        let top_index = builder.build_int_sub(len_val, one_i64, "top_index")?;

        // data[top_index]
        let data_gep = builder.build_struct_gep2(stack_type, param_stack, 0, "data_ptr")?;
        let data_ptr = builder
            .build_load2(
                self.type_helper.vm_value_type.ptr_type(AddressSpace::default()),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let elem_ptr =
            builder.build_in_bounds_gep2(self.type_helper.vm_value_type, data_ptr, &[top_index], "elem_ptr")?;

        builder.build_memcpy(
            param_out,
            8,
            elem_ptr,
            8,
            self.type_helper.vm_value_type.size_of().unwrap(),
        )?;
        builder.build_store(ret_val_ptr, self.type_helper.i32_type().const_zero())?;
        builder.build_unconditional_branch(return_bb)?;

        // return
        builder.position_at_end(return_bb);
        let ret_val = builder.build_load2(self.type_helper.i32_type(), ret_val_ptr, "ret")?;
        builder.build_return(Some(&ret_val.into_int_value()))?;

        Ok(function)
    }
}

/// 寄存器模块构建器
struct RegisterModuleBuilder<'a, 'b> {
    type_helper: &'b TypeHelper<'a>,
    max_registers: u32,
}

impl<'a, 'b> RegisterModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b TypeHelper<'a>, max_registers: u32) -> Self {
        Self {
            type_helper,
            max_registers,
        }
    }

    fn build(self, module: &mut Module<'a>) -> Result<VMRegisterModule<'a>> {
        let array_type = self.type_helper.vm_value_type.array_type(self.max_registers + 1);
        let set_value = self.create_set_value_function(module, array_type)?;

        Ok(VMRegisterModule { array_type, set_value })
    }

    fn create_set_value_function(
        &self,
        module: &mut Module<'a>,
        array_type: ArrayType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let array_ptr = array_type.ptr_type(AddressSpace::default());
        let fn_type = self.type_helper.void_type().fn_type(
            &[
                array_ptr.into(),
                self.type_helper.i32_type().into(),
                self.type_helper.vm_value_type.into(),
            ],
            false,
        );

        let function = module.add_function("avm_set_register_value", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 实现寄存器写入逻辑
        let builder = self.type_helper.context.create_builder();

        // 参数
        let param_array = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_index = function.get_nth_param(1).unwrap().into_int_value();
        let param_value = function.get_nth_param(2).unwrap().into_struct_value();

        // 基本块
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let oob_bb = self.type_helper.context.append_basic_block(function, "oob"); // 越界
        let in_bounds = self.type_helper.context.append_basic_block(function, "in_bounds"); // 合法

        builder.position_at_end(entry);

        // 检测越界：index >= array_len
        let array_len = self.type_helper.i32_type().const_int(array_type.len() as u64, false);
        let cond = builder.build_int_compare(IntPredicate::UGE, param_index, array_len, "index_oob")?;
        builder.build_conditional_branch(cond, oob_bb, in_bounds)?;

        // 越界分支：unreachable
        builder.position_at_end(oob_bb);
        builder.build_unreachable()?;

        // 写入分支
        builder.position_at_end(in_bounds);
        let i32_zero = self.type_helper.i32_type().const_zero();
        let elem_ptr = builder.build_in_bounds_gep2(array_type, param_array, &[i32_zero, param_index], "elem_ptr")?;
        builder.build_store(elem_ptr, param_value)?;
        builder.build_return(None)?;

        Ok(function)
    }
}

/// 内存模块构建器
struct MemoryModuleBuilder<'a, 'b> {
    type_helper: &'b TypeHelper<'a>,
}

impl<'a, 'b> MemoryModuleBuilder<'a, 'b> {
    fn new(type_helper: &'b TypeHelper<'a>) -> Self {
        Self { type_helper }
    }

    fn build(
        self,
        module: &mut Module<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
    ) -> Result<VMMemoryModule<'a>> {
        let memory_type = self.create_memory_type();
        let functions = self.create_memory_functions(module, memory_type, value_type_map)?;

        Ok(VMMemoryModule { memory_type, functions })
    }

    fn create_memory_type(&self) -> StructType<'a> {
        self.type_helper.context.struct_type(
            &[
                self.type_helper.ptr_type().into(), // data
                self.type_helper.i64_type().into(), // size
                self.type_helper.i64_type().into(), // next_addr
            ],
            false,
        )
    }

    fn create_memory_functions(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
    ) -> Result<MemoryFunctions<'a>> {
        let ensure_fn = self.create_ensure_function(module, memory_type)?;
        Ok(MemoryFunctions {
            init: self.create_init_function(module, memory_type)?,
            destroy: self.create_destroy_function(module, memory_type)?,
            ensure: ensure_fn,
            alloc: self.create_alloc_function(module, memory_type)?,
            store_value: self.create_store_value_function(module, memory_type, value_type_map, ensure_fn)?,
        })
    }

    fn create_init_function(&self, module: &mut Module<'a>, memory_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = self.type_helper.void_type().fn_type(&[memory_ptr.into()], false);
        let function = module.add_function("avm_mem_init", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();

        // 初始化内存结构
        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;

        let data_ptr = builder.build_array_malloc(
            self.type_helper.i8_type(),
            self.type_helper.i32_type().const_int(AVM_MEM_DEFAULT_SIZE, false),
            "data_ptr",
        )?;

        builder.build_store(data_gep, data_ptr)?;
        builder.build_store(
            size_gep,
            self.type_helper.i64_type().const_int(AVM_MEM_DEFAULT_SIZE, false),
        )?;
        builder.build_store(next_addr_gep, self.type_helper.i64_type().const_int(0x1000, false))?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_destroy_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = self.type_helper.void_type().fn_type(&[memory_ptr.into()], false);
        let function = module.add_function("avm_mem_free", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();

        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let data_value = builder
            .build_load2(self.type_helper.ptr_type(), data_gep, "data")?
            .into_pointer_value();
        builder.build_free(data_value)?;

        // 重置所有字段
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;

        builder.build_store(size_gep, self.type_helper.i64_type().const_zero())?;
        builder.build_store(next_addr_gep, self.type_helper.i64_type().const_zero())?;
        builder.build_return(None)?;

        Ok(function)
    }

    fn create_ensure_function(
        &self,
        module: &mut Module<'a>,
        memory_type: StructType<'a>,
    ) -> Result<FunctionValue<'a>> {
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .void_type()
            .fn_type(&[memory_ptr.into(), self.type_helper.i64_type().into()], false);
        let function = module.add_function("avm_mem_ensure", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();

        // ensure实现
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let check_size = self.type_helper.context.append_basic_block(function, "check_size");
        let expand_memory = self.type_helper.context.append_basic_block(function, "expand_memory");
        let return_bb = self.type_helper.context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_need = function.get_nth_param(1).unwrap().into_int_value();

        builder.build_unconditional_branch(check_size)?;

        builder.position_at_end(check_size);
        let size_gep = builder.build_struct_gep2(memory_type, param_memory, 1, "size_ptr")?;
        let size_value = builder
            .build_load2(self.type_helper.i64_type(), size_gep, "size")?
            .into_int_value();

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
        // new_size = current_size
        // while (new_size < needed) new_size += new_size/2 + PAGE_SIZE
        let new_size_ptr = builder.build_alloca(self.type_helper.i64_type(), "new_size")?;
        builder.build_store(new_size_ptr, current_size)?;

        let loop_cond = self
            .type_helper
            .context
            .append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_cond");
        let loop_body = self
            .type_helper
            .context
            .append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_body");
        let loop_end = self
            .type_helper
            .context
            .append_basic_block(builder.get_insert_block().unwrap().get_parent().unwrap(), "loop_end");

        builder.build_unconditional_branch(loop_cond)?;

        builder.position_at_end(loop_cond);
        let new_size_val = builder
            .build_load2(self.type_helper.i64_type(), new_size_ptr, "new_size_val")?
            .into_int_value();
        let need_more = builder.build_int_compare(IntPredicate::ULT, new_size_val, needed, "need_more")?;
        builder.build_conditional_branch(need_more, loop_body, loop_end)?;

        builder.position_at_end(loop_body);
        let half =
            builder.build_int_unsigned_div(new_size_val, self.type_helper.i64_type().const_int(2, false), "half")?;
        let increment = builder.build_int_add(
            half,
            self.type_helper.i64_type().const_int(AVM_PAGE_SIZE, false),
            "increment",
        )?;
        let updated_size = builder.build_int_add(new_size_val, increment, "updated_size")?;
        builder.build_store(new_size_ptr, updated_size)?;
        builder.build_unconditional_branch(loop_cond)?;

        builder.position_at_end(loop_end);
        let final_size = builder
            .build_load2(self.type_helper.i64_type(), new_size_ptr, "final_size")?
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
        // 分配新内存并复制旧数据
        let data_gep = builder.build_struct_gep2(memory_type, memory_ptr, 0, "data_ptr")?;
        let size_gep = builder.build_struct_gep2(memory_type, memory_ptr, 1, "size_ptr")?;

        let old_data = builder
            .build_load2(self.type_helper.ptr_type(), data_gep, "old_data")?
            .into_pointer_value();
        let old_size = builder
            .build_load2(self.type_helper.i64_type(), size_gep, "old_size")?
            .into_int_value();

        let new_data = builder.build_array_malloc(self.type_helper.i8_type(), new_size, "new_data")?;
        builder.build_memcpy(new_data, 8, old_data, 8, old_size)?;
        builder.build_free(old_data)?;

        builder.build_store(data_gep, new_data)?;
        builder.build_store(size_gep, new_size)?;

        Ok(())
    }

    fn create_alloc_function(&self, module: &mut Module<'a>, memory_type: StructType<'a>) -> Result<FunctionValue<'a>> {
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let fn_type = self
            .type_helper
            .i64_type()
            .fn_type(&[memory_ptr.into(), self.type_helper.i64_type().into()], false);
        let function = module.add_function("avm_mem_alloc", fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_size = function.get_nth_param(1).unwrap().into_int_value();

        let next_addr_gep = builder.build_struct_gep2(memory_type, param_memory, 2, "next_addr_ptr")?;
        let next_addr = builder
            .build_load2(self.type_helper.i64_type(), next_addr_gep, "next_addr")?
            .into_int_value();

        let new_next_addr = builder.build_int_add(next_addr, param_size, "new_next_addr")?;
        let need_with_slack = builder.build_int_add(
            new_next_addr,
            self.type_helper.i64_type().const_int(AVM_ALLOC_SLACK, false),
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
    ) -> Result<FunctionValue<'a>> {
        let memory_ptr = memory_type.ptr_type(AddressSpace::default());
        let vm_value_ptr = self.type_helper.vm_value_type.ptr_type(AddressSpace::default());

        let fn_type = self.type_helper.void_type().fn_type(
            &[
                memory_ptr.into(),
                self.type_helper.i64_type().into(),
                vm_value_ptr.into(),
            ],
            false,
        );
        let function = module.add_function("avm_mem_store_value", fn_type, None);
        function.add_attribute(AttributeLoc::Function, self.type_helper.inline_attr);

        // 使用简化的store_value实现
        let builder = self.type_helper.context.create_builder();
        let entry = self.type_helper.context.append_basic_block(function, "entry");
        let store_tag = self.type_helper.context.append_basic_block(function, "store_tag");
        let store_value = self.type_helper.context.append_basic_block(function, "store_value");
        let return_bb = self.type_helper.context.append_basic_block(function, "return");

        builder.position_at_end(entry);

        let param_memory = function.get_nth_param(0).unwrap().into_pointer_value();
        let param_addr = function.get_nth_param(1).unwrap().into_int_value();
        let param_value = function.get_nth_param(2).unwrap().into_pointer_value();

        // 读取类型标签
        let tag_gep = builder.build_struct_gep2(self.type_helper.vm_value_type, param_value, 0, "tag_ptr")?;
        let tag_value = builder
            .build_load2(self.type_helper.i8_type(), tag_gep, "tag")?
            .into_int_value();

        // 计算值大小
        let value_size = self.get_value_size_for_tag(&builder, tag_value, value_type_map, entry, function)?;

        // 确保内存足够
        let total_need = builder.build_int_add(
            param_addr,
            builder.build_int_z_extend(value_size, self.type_helper.i64_type(), "size_i64")?,
            "need",
        )?;
        let need_plus_tag = builder.build_int_add(
            total_need,
            self.type_helper.i64_type().const_int(1, false),
            "need_plus_tag",
        )?;

        builder.build_call(ensure_fn, &[param_memory.into(), need_plus_tag.into()], "")?;

        builder.build_unconditional_branch(store_tag)?;

        builder.position_at_end(store_tag);
        // 存储类型标签在addr-1位置
        let data_gep = builder.build_struct_gep2(memory_type, param_memory, 0, "data_ptr")?;
        let data_ptr = builder
            .build_load2(self.type_helper.ptr_type(), data_gep, "data")?
            .into_pointer_value();

        let tag_offset = builder.build_int_sub(
            param_addr,
            self.type_helper.i64_type().const_int(1, false),
            "tag_offset",
        )?;
        let tag_dest = builder.build_in_bounds_gep2(self.type_helper.i8_type(), data_ptr, &[tag_offset], "tag_dest")?;

        let tag_byte = builder.build_int_truncate(tag_value, self.type_helper.i8_type(), "tag_byte")?;
        builder.build_store(tag_dest, tag_byte)?;

        // 检查是否需要存储值数据
        let is_zero_size = builder.build_int_compare(
            IntPredicate::EQ,
            value_size,
            self.type_helper.i32_type().const_zero(),
            "is_zero_size",
        )?;
        builder.build_conditional_branch(is_zero_size, return_bb, store_value)?;

        builder.position_at_end(store_value);
        // 存储值数据
        let value_gep = builder.build_struct_gep2(self.type_helper.vm_value_type, param_value, 1, "value_ptr")?;
        let value_dest =
            builder.build_in_bounds_gep2(self.type_helper.i8_type(), data_ptr, &[param_addr], "value_dest")?;

        builder.build_memcpy(value_dest, 8, value_gep, 8, value_size)?;
        builder.build_unconditional_branch(return_bb)?;

        builder.position_at_end(return_bb);
        builder.build_return(None)?;

        Ok(function)
    }

    fn get_value_size_for_tag(
        &self,
        builder: &Builder<'a>,
        tag_value: IntValue<'a>,
        value_type_map: &HashMap<BytecodeValueType, u8>,
        current_basic_block: BasicBlock,
        current_function: FunctionValue,
    ) -> Result<IntValue<'a>> {
        // 使用查找表或switch
        // 这里使用简单的条件判断，实际应该使用更高效的方法
        let size_ptr = builder.build_alloca(self.type_helper.i32_type(), "size")?;

        // 默认大小为8字节（适用于大多数类型）
        builder.build_store(size_ptr, self.type_helper.i32_type().const_int(8, false))?;

        // 对于特殊大小的类型进行判断
        let tag_i32 = builder.build_int_z_extend(tag_value, self.type_helper.i32_type(), "tag_i32")?;

        let default_bb = self.type_helper.context.append_basic_block(current_function, "default");

        let mut cases = Vec::new();
        for (typ, val) in value_type_map {
            let case_bb = self
                .type_helper
                .context
                .append_basic_block(current_function, &format!("case_{:?}", typ));
            cases.push((self.type_helper.i32_type().const_int(*val as u64, false), case_bb));
            builder.position_at_end(case_bb);
            builder.build_store(
                size_ptr,
                self.type_helper.i32_type().const_int(typ.size() as u64, false),
            )?;
            builder.build_unconditional_branch(default_bb)?;
        }
        builder.position_at_end(current_basic_block);
        builder.build_switch(tag_i32, default_bb, &cases)?;

        builder.position_at_end(default_bb);
        let final_size = builder
            .build_load2(self.type_helper.i32_type(), size_ptr, "final_size")?
            .into_int_value();
        Ok(final_size)
    }
}

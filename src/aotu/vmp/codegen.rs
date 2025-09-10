use crate::aotu::vmp::bytecode::VMPBytecodeEncoder;
use crate::aotu::vmp::compiler::VMPCompilerContext;
use crate::aotu::vmp::isa::VMPOpcode;
use anyhow::{Result, anyhow};
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::BasicTypeEnum;
use llvm_plugin::inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue};
use log::{Level, debug, log_enabled};
use std::path::PathBuf;

pub struct VMPCodeGenerator<'a, 'b> {
    module: &'b mut Module<'a>,
    runtime_functions: RuntimeFunctions<'a>,
    encoder: VMPBytecodeEncoder,
}

struct RuntimeFunctions<'a> {
    avm_runtime_new: FunctionValue<'a>,
    avm_runtime_destroy: FunctionValue<'a>,
    avm_runtime_execute: FunctionValue<'a>,
}

impl<'a, 'b> VMPCodeGenerator<'a, 'b> {
    pub fn new(module: &'b mut Module<'a>) -> Result<Self> {
        let runtime_functions = Self::declare_runtime_functions(module)?;

        Ok(Self {
            module,
            runtime_functions,
            encoder: VMPBytecodeEncoder::new(),
        })
    }

    fn declare_runtime_functions<'l>(module: &Module<'l>) -> Result<RuntimeFunctions<'l>> {
        let context = module.get_context();

        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let ptr_type = context.ptr_type(AddressSpace::default());

        // 不使用 alwaysinline 属性，让运行时函数保持正常调用
        // let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        // 声明 AVM runtime 函数
        let avm_runtime_new_type = ptr_type.fn_type(&[], false); // 返回运行时实例指针
        let avm_runtime_new = module.add_function("avm_runtime_new", avm_runtime_new_type, None);

        let avm_runtime_destroy_type = void_type.fn_type(&[ptr_type.into()], false);
        let avm_runtime_destroy = module.add_function("avm_runtime_destroy", avm_runtime_destroy_type, None);

        // avm_runtime_execute(runtime_ptr, bytecode_ptr, bytecode_length) -> i64
        let avm_runtime_execute_type = i64_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        let avm_runtime_execute = module.add_function("avm_runtime_execute", avm_runtime_execute_type, None);

        Ok(RuntimeFunctions {
            avm_runtime_new,
            avm_runtime_destroy,
            avm_runtime_execute,
        })
    }

    pub(crate) fn generate_runtime_init(&self) -> Result<()> {
        if log_enabled!(Level::Debug) {
            debug!("Generating runtime initialization functions");
        }
        // 在这里我们只是声明了函数，实际的运行时实现需要在链接时提供
        // 或者可以在这里添加运行时构造函数的调用（如果需要）
        Ok(())
    }

    /// 将虚拟机指令序列编译成调用虚拟机运行时的LLVM IR
    pub fn compile_function_to_vm_call(&mut self, function: FunctionValue, context: VMPCompilerContext) -> Result<()> {
        // 序列化指令数据到全局常量
        let instructions_data = self.serialize_instructions_to_global(context)?;

        // 清空原函数体，重新生成调用虚拟机的代码
        self.replace_function_body_with_vm_call(function, instructions_data)?;

        Ok(())
    }

    /// 序列化指令数据到全局常量
    fn serialize_instructions_to_global(&mut self, compiler_context: VMPCompilerContext) -> Result<GlobalValue<'a>> {
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
    ) -> Result<()> {
        // todo
        Ok(())
    }
}

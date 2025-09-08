use crate::aotu::vmp::avm::AVMOpcode;
use crate::aotu::vmp::bytecode::BytecodeEncoder;
use anyhow::{Result, anyhow};
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::BasicTypeEnum;
use llvm_plugin::inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue};
use log::debug;

pub struct AVMCodeGenerator<'a, 'b> {
    context: ContextRef<'a>,
    module: &'b mut Module<'a>,
    builder: Builder<'a>,
    runtime_functions: RuntimeFunctions<'a>,
}

struct RuntimeFunctions<'a> {
    avm_runtime_new: FunctionValue<'a>,
    avm_runtime_destroy: FunctionValue<'a>,
    avm_runtime_execute: FunctionValue<'a>,
}

impl<'a, 'b> AVMCodeGenerator<'a, 'b> {
    pub fn new(module: &'b mut Module<'a>) -> Result<Self> {
        let context = module.get_context();
        let builder = context.create_builder();
        let runtime_functions = Self::declare_runtime_functions(&context, module)?;

        Ok(Self {
            context,
            module,
            builder,
            runtime_functions,
        })
    }

    fn declare_runtime_functions<'l>(context: &ContextRef<'l>, module: &Module<'l>) -> Result<RuntimeFunctions<'l>> {
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

    pub(crate) fn generate_runtime_init(&self) -> anyhow::Result<()> {
        debug!("Generating runtime initialization functions");
        // 在这里我们只是声明了函数，实际的运行时实现需要在链接时提供
        // 或者可以在这里添加运行时构造函数的调用（如果需要）
        Ok(())
    }

    /// 将虚拟机指令序列编译成调用虚拟机运行时的LLVM IR
    pub fn compile_function_to_vm_call(&self, function: FunctionValue, instructions: &[AVMOpcode]) -> Result<()> {
        debug!(
            "Compiling function {:?} to VM call with {} instructions",
            function.get_name(),
            instructions.len()
        );

        // 序列化指令数据到全局常量
        let instructions_data = self.serialize_instructions_to_global(instructions)?;

        // 清空原函数体，重新生成调用虚拟机的代码
        self.replace_function_body_with_vm_call(function, instructions_data, instructions.len())?;

        Ok(())
    }

    /// 序列化指令数据到全局常量
    fn serialize_instructions_to_global(&self, instructions: &[AVMOpcode]) -> Result<GlobalValue<'a>> {
        // 使用字节码编码器
        let mut encoder = BytecodeEncoder::new();
        let bytecode_data = encoder
            .encode_instructions(instructions)
            .map_err(|e| anyhow!("Failed to serialize instructions to bytecode: {}", e))?;

        debug!("Serialized instructions to {} bytes of bytecode", bytecode_data.len());

        // 创建全局字节数组常量
        let global_name = format!("__avm_bytecode_{}", rand::random::<u32>());
        let global_value = self.module.add_global(
            self.context.i8_type().array_type(bytecode_data.len() as u32),
            Some(AddressSpace::default()),
            &global_name,
        );

        // 设置初始值
        let byte_values: Vec<_> = bytecode_data
            .iter()
            .map(|&b| self.context.i8_type().const_int(b as u64, false))
            .collect();
        let array_value = self.context.i8_type().const_array(&byte_values);
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
        _instruction_count: usize,
    ) -> Result<()> {
        // todo
        Ok(())
    }

    /// 处理函数返回值
    fn handle_function_return(&self, function: FunctionValue, vm_result: BasicValueEnum) -> Result<()> {
        let return_type = function.get_type().get_return_type();

        match return_type {
            Some(ret_type) => {
                // 函数有返回值，需要从vm_result转换
                match ret_type {
                    BasicTypeEnum::IntType(int_type) => {
                        // 将i64结果转换为目标整数类型
                        let casted_result = if int_type.get_bit_width() == 64 {
                            vm_result.into_int_value()
                        } else {
                            self.builder
                                .build_int_truncate(vm_result.into_int_value(), int_type, "result_cast")
                                .map_err(|e| anyhow!("Failed to truncate int: {}", e))?
                        };

                        self.builder
                            .build_return(Some(&casted_result))
                            .map_err(|e| anyhow!("Failed to build return: {}", e))?;
                    },
                    BasicTypeEnum::FloatType(_) => {
                        // 浮点数需要特殊处理，这里简化为返回0
                        let zero = ret_type.const_zero();
                        self.builder
                            .build_return(Some(&zero))
                            .map_err(|e| anyhow!("Failed to build return: {}", e))?;
                    },
                    BasicTypeEnum::PointerType(ptr_type) => {
                        // 指针类型，将i64转换为指针
                        let casted_result = self
                            .builder
                            .build_int_to_ptr(vm_result.into_int_value(), ptr_type, "result_ptr")
                            .map_err(|e| anyhow!("Failed to cast int to ptr: {}", e))?;

                        self.builder
                            .build_return(Some(&casted_result))
                            .map_err(|e| anyhow!("Failed to build return: {}", e))?;
                    },
                    _ => {
                        return Err(anyhow!("Unsupported return type: {:?}", ret_type));
                    },
                }
            },
            None => {
                // void函数，直接返回
                self.builder
                    .build_return(None)
                    .map_err(|e| anyhow!("Failed to build void return: {}", e))?;
            },
        }

        Ok(())
    }
}

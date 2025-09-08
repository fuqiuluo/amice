use crate::aotu::vmp::avm::AVMOpcode;
use anyhow::Result;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::Attribute;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::context::{Context, ContextRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::{BasicType, BasicTypeEnum};
use llvm_plugin::inkwell::values::{FunctionValue, PointerValue};

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
        let i8_type = context.i8_type();
        let i32_type = context.i32_type();
        let i64_type = context.i64_type();
        let void_type = context.void_type();
        let ptr_type = i8_type.ptr_type(AddressSpace::default());

        let inline_attr = context.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);

        // 声明 AVM runtime 函数
        let avm_runtime_new_type = void_type.fn_type(&[], false);
        let avm_runtime_new = module.add_function("avm_runtime_new", avm_runtime_new_type, None);
        avm_runtime_new.add_attribute(llvm_plugin::inkwell::attributes::AttributeLoc::Function, inline_attr);

        let avm_runtime_destroy_type = void_type.fn_type(&[ptr_type.into()], false);
        let avm_runtime_destroy = module.add_function("avm_runtime_destroy", avm_runtime_destroy_type, None);
        avm_runtime_destroy.add_attribute(llvm_plugin::inkwell::attributes::AttributeLoc::Function, inline_attr);

        let avm_runtime_execute_type = i64_type.fn_type(&[ptr_type.into(), ptr_type.into(), i64_type.into()], false);
        let avm_runtime_execute = module.add_function("avm_runtime_execute", avm_runtime_execute_type, None);
        avm_runtime_execute.add_attribute(llvm_plugin::inkwell::attributes::AttributeLoc::Function, inline_attr);

        Ok(RuntimeFunctions {
            avm_runtime_new,
            avm_runtime_destroy,
            avm_runtime_execute,
        })
    }

    /// 将虚拟机指令序列编译成调用虚拟机运行时的LLVM IR
    pub fn compile_function_to_vm_call(&self, function: FunctionValue<'a>, instructions: &[AVMOpcode]) -> Result<()> {
        // // 编码指令序列
        // let encoded_instructions = encode_instructions(instructions)?;
        //
        // // 清空函数体，重新生成
        // self.clear_function_body(function);
        //
        // // 创建新的入口基本块
        // let entry_block = self.context.append_basic_block(function, "entry");
        // self.builder.position_at_end(entry_block);
        //
        // // 创建虚拟机运行时实例
        // let runtime_ptr = self.builder.build_call(
        //     self.runtime_functions.avm_runtime_new,
        //     &[],
        //     "runtime"
        // )?;
        // let runtime_ptr = runtime_ptr.try_as_basic_value().left()
        //     .ok_or_else(|| anyhow!("Failed to get runtime pointer"))?
        //     .into_pointer_value();
        //
        // // 分配内存存储指令序列
        // let instructions_size = self.context.i64_type().const_int(encoded_instructions.len() as u64, false);
        // let instructions_ptr = self.builder.build_call(
        //     self.runtime_functions.malloc,
        //     &[instructions_size.into()],
        //     "instructions_ptr"
        // )?;
        // let instructions_ptr = instructions_ptr.try_as_basic_value().left()
        //     .ok_or_else(|| anyhow!("Failed to get instructions pointer"))?
        //     .into_pointer_value();
        //
        // // 将指令数据写入内存
        // self.write_instructions_to_memory(instructions_ptr, &encoded_instructions)?;
        //
        // // 调用虚拟机执行指令
        // let result = self.builder.build_call(
        //     self.runtime_functions.avm_runtime_execute,
        //     &[
        //         runtime_ptr.into(),
        //         instructions_ptr.into(),
        //         instructions_size.into(),
        //     ],
        //     "vm_result"
        // )?;
        //
        // // 清理资源
        // self.builder.build_call(
        //     self.runtime_functions.free,
        //     &[instructions_ptr.into()],
        //     ""
        // )?;
        //
        // self.builder.build_call(
        //     self.runtime_functions.avm_runtime_destroy,
        //     &[runtime_ptr.into()],
        //     ""
        // )?;
        //
        // // 处理返回值
        // let return_type = function.get_type().get_return_type();
        // match return_type {
        //     Some(ret_type) => {
        //         let result_value = result.try_as_basic_value().left()
        //             .ok_or_else(|| anyhow!("Failed to get result value"))?;
        //
        //         // 根据函数返回类型转换结果
        //         let converted_result = self.convert_vm_result_to_function_return(result_value, ret_type)?;
        //         self.builder.build_return(Some(&converted_result))?;
        //     }
        //     None => {
        //         // void 函数
        //         self.builder.build_return(None)?;
        //     }
        // }
        //
        // Ok(())
        unimplemented!()
    }

    fn clear_function_body(&self, function: FunctionValue<'a>) {
        // 删除所有基本块
        unsafe {
            let mut current_bb = function.get_first_basic_block();
            while let Some(bb) = current_bb {
                let next_bb = bb.get_next_basic_block();
                bb.delete().unwrap();
                current_bb = next_bb;
            }
        }
    }

    fn write_instructions_to_memory(&self, ptr: PointerValue<'a>, data: &[u8]) -> Result<()> {
        // let i8_type = self.context.i8_type();
        //
        // for (i, &byte) in data.iter().enumerate() {
        //     let byte_value = i8_type.const_int(byte as u64, false);
        //     let offset = self.context.i64_type().const_int(i as u64, false);
        //
        //     unsafe {
        //         let byte_ptr = self.builder.build_gep(
        //             i8_type,
        //             ptr,
        //             &[offset],
        //             &format!("byte_ptr_{}", i)
        //         )?;
        //         self.builder.build_store(byte_ptr, byte_value)?;
        //     }
        // }
        //
        // Ok(())
        unimplemented!()
    }

    fn convert_vm_result_to_function_return(
        &self,
        vm_result: llvm_plugin::inkwell::values::BasicValueEnum<'a>,
        target_type: BasicTypeEnum<'a>,
    ) -> Result<llvm_plugin::inkwell::values::BasicValueEnum<'a>> {
        // let vm_result_int = vm_result.into_int_value();
        //
        // match target_type {
        //     BasicTypeEnum::IntType(int_type) => {
        //         match int_type.get_bit_width() {
        //             1 => {
        //                 // bool
        //                 let zero = self.context.i64_type().const_zero();
        //                 let is_nonzero = self.builder.build_int_compare(
        //                     llvm_plugin::inkwell::IntPredicate::NE,
        //                     vm_result_int,
        //                     zero,
        //                     "is_nonzero"
        //                 )?;
        //                 Ok(is_nonzero.into())
        //             }
        //             8 => {
        //                 let truncated = self.builder.build_int_truncate(
        //                     vm_result_int,
        //                     self.context.i8_type(),
        //                     "trunc_i8"
        //                 )?;
        //                 Ok(truncated.into())
        //             }
        //             16 => {
        //                 let truncated = self.builder.build_int_truncate(
        //                     vm_result_int,
        //                     self.context.i16_type(),
        //                     "trunc_i16"
        //                 )?;
        //                 Ok(truncated.into())
        //             }
        //             32 => {
        //                 let truncated = self.builder.build_int_truncate(
        //                     vm_result_int,
        //                     self.context.i32_type(),
        //                     "trunc_i32"
        //                 )?;
        //                 Ok(truncated.into())
        //             }
        //             64 => Ok(vm_result_int.into()),
        //             _ => Err(anyhow!("Unsupported integer bit width: {}", int_type.get_bit_width())),
        //         }
        //     }
        //     BasicTypeEnum::FloatType(float_type) => {
        //         match float_type.get_bit_width() {
        //             32 => {
        //                 let int_as_float_bits = self.builder.build_int_truncate(
        //                     vm_result_int,
        //                     self.context.i32_type(),
        //                     "float_bits"
        //                 )?;
        //                 let float_val = self.builder.build_bitcast(
        //                     int_as_float_bits,
        //                     self.context.f32_type(),
        //                     "int_to_float"
        //                 )?.into_float_value();
        //                 Ok(float_val.into())
        //             }
        //             64 => {
        //                 let float_val = self.builder.build_bitcast(
        //                     vm_result_int,
        //                     self.context.f64_type(),
        //                     "int_to_double"
        //                 )?.into_float_value();
        //                 Ok(float_val.into())
        //             }
        //             _ => Err(anyhow!("Unsupported float bit width: {}", float_type.get_bit_width())),
        //         }
        //     }
        //     BasicTypeEnum::PointerType(_) => {
        //         let ptr_val = self.builder.build_int_to_ptr(
        //             vm_result_int,
        //             target_type.into_pointer_type(),
        //             "int_to_ptr"
        //         )?;
        //         Ok(ptr_val.into())
        //     }
        //     _ => Err(anyhow!("Unsupported return type: {:?}", target_type)),
        // }
        unimplemented!()
    }

    /// 为整个模块生成虚拟机运行时初始化代码
    pub fn generate_runtime_init(&self) -> Result<()> {
        // 可以在这里添加全局构造函数来初始化虚拟机运行时
        // 或者注册必要的外部函数等
        Ok(())
    }

    /// 将函数参数转换为虚拟机值并注入到指令序列中
    pub fn inject_function_arguments(
        &self,
        function: FunctionValue<'a>,
        instructions: &mut Vec<AVMOpcode>,
    ) -> Result<()> {
        // 在指令序列开头注入参数加载指令
        let mut arg_instructions = Vec::new();

        for (i, param) in function.get_param_iter().enumerate() {
            // 为每个参数创建寄存器分配指令
            arg_instructions.push(AVMOpcode::PopToReg { reg: i as u32 });
        }

        // 将参数指令插入到序列开头
        arg_instructions.extend(instructions.iter().cloned());
        *instructions = arg_instructions;

        Ok(())
    }
}

/// 辅助函数：从 AVMValue 创建 LLVM 常量
pub fn avm_value_to_llvm_const<'a>(
    context: &'a Context,
    value: &crate::aotu::vmp::avm::AVMValue,
) -> llvm_plugin::inkwell::values::BasicValueEnum<'a> {
    // match value {
    //     crate::aotu::vmp::avm::AVMValue::I1(v) => {
    //         context.bool_type().const_int(if *v { 1 } else { 0 }, false).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::I8(v) => {
    //         context.i8_type().const_int(*v as u64, true).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::I16(v) => {
    //         context.i16_type().const_int(*v as u64, true).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::I32(v) => {
    //         context.i32_type().const_int(*v as u64, true).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::I64(v) => {
    //         context.i64_type().const_int(*v as u64, true).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::F32(v) => {
    //         context.f32_type().const_float(*v as f64).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::F64(v) => {
    //         context.f64_type().const_float(*v).into()
    //     }
    //     crate::aotu::vmp::avm::AVMValue::Ptr(v) => {
    //         let int_val = context.i64_type().const_int(*v as u64, false);
    //         context.i8_type().ptr_type(AddressSpace::default()).const_null().into()
    //     }
    // }
    unimplemented!()
}

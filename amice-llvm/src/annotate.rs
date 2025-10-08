use inkwell::llvm_sys::core::{LLVMGetAsString, LLVMGetNumOperands, LLVMGetOperand, LLVMGetSection};
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, BasicValueEnum, GlobalValue, StructValue};
use inkwell::{
    module::Module,
    values::FunctionValue,
};
use std::ffi::{CStr, CString, c_uint};

/// 读取给定函数在 llvm.global.annotations 里的注解
pub(crate) fn read_function_annotate<'ctx>(module: &Module<'ctx>, func: FunctionValue<'ctx>) -> Result<Vec<String>, &'static str> {
    let mut out = Vec::new();

    let Some(global) = module.get_global("llvm.global.annotations") else {
        return Ok(out);
    };

    let Some(ca) = global.get_initializer() else {
        return Ok(out);
    };
    let ca_ref = ca.as_value_ref();

    unsafe {
        let num_operands = LLVMGetNumOperands(ca_ref as LLVMValueRef);
        for i in 0..num_operands {
            let elem = LLVMGetOperand(ca_ref as LLVMValueRef, i as c_uint);
            if elem.is_null() {
                continue;
            }

            let constant_struct = StructValue::new(elem);
            if constant_struct.is_null() || constant_struct.count_fields() < 2 {
                continue;
            }

            if let Some(first_field) = constant_struct.get_field_at_index(0)
                && first_field.is_pointer_value()
                && first_field.as_value_ref() == func.as_value_ref()
            {
                for j in 1..constant_struct.count_fields() {
                    let Some(field) = constant_struct.get_field_at_index(j) else {
                        continue;
                    };

                    if !field.is_pointer_value() || field.into_pointer_value().is_null() {
                        continue;
                    }

                    let section = CStr::from_ptr(LLVMGetSection(field.as_value_ref() as LLVMValueRef));
                    let section = section.to_str().unwrap().to_string().to_lowercase();

                    if section != "llvm.metadata" {
                        continue;
                    }

                    let global_string = GlobalValue::new(field.as_value_ref());
                    let Some(str_arr) = (match global_string
                        .get_initializer()
                        .ok_or("Invalid string field: initializer needed")?
                    {
                        BasicValueEnum::ArrayValue(arr) => Some(arr),
                        BasicValueEnum::StructValue(stru) if stru.count_fields() <= 1 => {
                            match stru.get_field_at_index(0).ok_or("Invalid string field")? {
                                BasicValueEnum::ArrayValue(arr) => Some(arr),
                                _ => None,
                            }
                        },
                        _ => None,
                    }) else {
                        eprintln!("Invalid string field: {:?}", field);
                        continue;
                    };

                    let mut len = 0;
                    let ptr = LLVMGetAsString(str_arr.as_value_ref(), &mut len);
                    if ptr.is_null() {
                        continue;
                    }
                    let arr = std::slice::from_raw_parts::<u8>(ptr.cast(), len - 1);
                    let c_str = CString::new(arr).unwrap();
                    out.push(c_str.to_string_lossy().into_owned());
                }
            } else {
                continue;
            }
        }
    }

    Ok(out)
}

// @.str = private unnamed_addr constant [18 x i8] c"add(10, 20) = %d\0A\00", align 1
// @.str.1 = private unnamed_addr constant [21 x i8] c"multiply(5, 6) = %d\0A\00", align 1
// @.str.2 = private unnamed_addr constant [20 x i8] c"custom_calling_conv\00", section "llvm.metadata"
// @.str.3 = private unnamed_addr constant [8 x i8] c"test1.c\00", section "llvm.metadata"
// @llvm.global.annotations = appending global [2 x { ptr, ptr, ptr, i32, ptr }] [{ ptr, ptr, ptr, i32, ptr } { ptr @add, ptr @.str.2, ptr @.str.3, i32 6, ptr null }, { ptr, ptr, ptr, i32, ptr } { ptr @multiply, ptr @.str.2, ptr @.str.3, i32 11, ptr null }], section "llvm.metadata"

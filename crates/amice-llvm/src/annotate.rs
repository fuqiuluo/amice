use inkwell::llvm_sys::{LLVMOpcode, core::*, prelude::LLVMValueRef};
use inkwell::{
    module::Module,
    values::{AsValueRef, FunctionValue},
};

/// Strip pointer wrappers used by annotation tables from typed-pointer LLVM.
/// Nonzero GEPs are deliberately not reinterpreted as the start of a string.
unsafe fn annotation_base(mut value: LLVMValueRef) -> LLVMValueRef {
    unsafe {
        for _ in 0..8 {
            if value.is_null() || LLVMIsAConstantExpr(value).is_null() {
                break;
            }
            let count = LLVMGetNumOperands(value);
            let transparent = match LLVMGetConstOpcode(value) {
                LLVMOpcode::LLVMBitCast | LLVMOpcode::LLVMAddrSpaceCast => count == 1,
                LLVMOpcode::LLVMGetElementPtr => {
                    count > 1
                        && (1..count).all(|n| {
                            let index = LLVMGetOperand(value, n as u32);
                            !LLVMIsAConstantInt(index).is_null() && LLVMIsNull(index) != 0
                        })
                },
                _ => false,
            };
            if !transparent {
                break;
            }
            value = LLVMGetOperand(value, 0);
        }
        value
    }
}

/// Read the annotation field only; filenames and optional arguments are not
/// configuration. Its section is optional and is never dereferenced as a C string.
pub(crate) fn read_function_annotate<'ctx>(
    module: &Module<'ctx>,
    func: FunctionValue<'ctx>,
) -> Result<Vec<String>, &'static str> {
    let mut out = Vec::new();
    let Some(global) = module.get_global("llvm.global.annotations") else {
        return Ok(out);
    };
    let Some(initializer) = global.get_initializer() else {
        return Ok(out);
    };
    unsafe {
        let array = initializer.as_value_ref();
        for n in 0..LLVMGetNumOperands(array) {
            let row = LLVMGetOperand(array, n as u32);
            if LLVMIsAConstantStruct(row).is_null() || LLVMGetNumOperands(row) < 2 {
                continue;
            }
            if annotation_base(LLVMGetOperand(row, 0)) != func.as_value_ref() {
                continue;
            }
            let string = annotation_base(LLVMGetOperand(row, 1));
            if LLVMIsAGlobalVariable(string).is_null() {
                continue;
            }
            let mut data = LLVMGetInitializer(string);
            if data.is_null() {
                continue;
            }
            // Some frontends wrap their byte array in a one-field struct.
            if !LLVMIsAConstantStruct(data).is_null() && LLVMGetNumOperands(data) == 1 {
                data = LLVMGetOperand(data, 0);
            }
            // LLVMIsConstantString casts to ConstantDataSequential internally;
            // empty/all-zero arrays may instead be ConstantAggregateZero.
            if LLVMIsAConstantDataSequential(data).is_null() || LLVMIsConstantString(data) == 0 {
                continue;
            }
            let mut len = 0;
            let bytes = LLVMGetAsString(data, &mut len);
            if bytes.is_null() || len == 0 {
                continue;
            }
            let bytes = std::slice::from_raw_parts(bytes.cast::<u8>(), len);
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            if end != 0 {
                out.push(String::from_utf8_lossy(&bytes[..end]).into_owned());
            }
        }
    }
    Ok(out)
}

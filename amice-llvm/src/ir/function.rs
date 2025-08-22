use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::{LLVMGetEntryBasicBlock, LLVMGetValueKind, LLVMIsABasicBlock};
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMModuleRef, LLVMValueRef};
use inkwell::types::{AsTypeRef, FunctionType};
use inkwell::values::{AsValueRef, FunctionValue, PointerValue};
use crate::ffi;
use crate::ffi::ArgReplacement;
use crate::ir::constants::get_bitcast_constant;

pub fn get_basic_block_entry_ref(fun: &FunctionValue) -> LLVMBasicBlockRef {
    unsafe { LLVMGetEntryBasicBlock(fun.as_value_ref()) }
}

/// Get the entry basic block of a function.
/// tips: If the function has only one basic block, the behavior of calling its get entry is UB
pub fn get_basic_block_entry(fun: FunctionValue) -> Option<BasicBlock> {
    unsafe {
        let basic_block = get_basic_block_entry_ref(&fun);
        if LLVMIsABasicBlock(basic_block as LLVMValueRef).is_null() {
            return None;
        }
        BasicBlock::new(get_basic_block_entry_ref(&fun))
    }
}

#[allow(dead_code)]
pub fn cast_ptr_to_fn_ptr<'a>(addr: PointerValue<'a>, function_type: FunctionType<'a>) -> Option<FunctionValue<'a>> {
    unsafe {
        let value = get_bitcast_constant(
            addr.as_value_ref() as *mut std::ffi::c_void,
            function_type.as_type_ref() as *mut std::ffi::c_void,
        ) as LLVMValueRef;
        FunctionValue::new(value)
    }
}

pub fn is_inline_marked_function(function: FunctionValue) -> bool {
    unsafe {
        ffi::amice_is_inline_marked_function(function.as_value_ref() as LLVMValueRef)
    }
}

pub fn clone_function(function_value: FunctionValue) -> Option<FunctionValue> {
    let clone = unsafe {
        ffi::amice_clone_function(function_value.as_value_ref() as LLVMValueRef)
    };

    unsafe {
        FunctionValue::new(clone)
    }
}

pub unsafe fn function_specialize_partial(
    module: LLVMModuleRef,
    original_func: LLVMValueRef,
    replacements: &[(u32, LLVMValueRef)],
) -> Result<LLVMValueRef, &'static str> {
    if original_func.is_null() {
        return Err("Null pointer");
    }

    let arg_replacements: Vec<ArgReplacement> = replacements
        .iter()
        .map(|(index, constant)| ArgReplacement {
            index: *index,
            constant: *constant,
        })
        .collect();

    let result = unsafe {
        ffi::amice_specialize_function(
            original_func,
            module,
            arg_replacements.as_ptr(),
            arg_replacements.len() as u32,
        )
    };

    if result.is_null() {
        Err("Specialization failed")
    } else {
        Ok(result)
    }
}

pub unsafe fn fix_stack(function:FunctionValue) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 0, 0)
}

pub unsafe fn fix_stack_at_terminator(function: FunctionValue) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 1, 0)
}

pub unsafe fn fix_stack_with_max_iterations(function: FunctionValue, max_iterations: usize) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 0, max_iterations as i32)
}
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::{LLVMGetEntryBasicBlock, LLVMGetValueKind, LLVMIsABasicBlock};
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::types::{AsTypeRef, FunctionType};
use inkwell::values::{AsValueRef, FunctionValue, PointerValue};
use crate::ffi;
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

pub unsafe fn fix_stack(function:FunctionValue) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 0, 0)
}

pub unsafe fn fix_stack_at_terminator(function: FunctionValue) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 1, 0)
}

pub unsafe fn fix_stack_with_max_iterations(function: FunctionValue, max_iterations: usize) {
    ffi::amice_fix_stack(function.as_value_ref() as LLVMValueRef, 0, max_iterations as i32)
}
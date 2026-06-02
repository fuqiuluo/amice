use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use std::ffi::{c_char, c_void};

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_basic_block_split(
        block: LLVMBasicBlockRef,
        inst: LLVMValueRef,
        name: *const c_char,
        before: i32,
    ) -> *mut c_void;

    pub(crate) fn amice_basic_block_first_insertion_pt(block: LLVMBasicBlockRef) -> LLVMValueRef;

    pub(crate) fn amice_basic_block_remove_predecessor(basic_block: LLVMBasicBlockRef, predecessor: LLVMBasicBlockRef);
}

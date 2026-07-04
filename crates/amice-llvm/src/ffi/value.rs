use std::ffi::c_char;

use inkwell::llvm_sys::prelude::LLVMValueRef;

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_value_replace_non_metadata_uses_with(value: LLVMValueRef, replacement: LLVMValueRef);

    pub(crate) fn amice_value_drop_droppable_uses(value: LLVMValueRef);

    pub(crate) fn amice_value_has_undroppable_uses(value: LLVMValueRef) -> bool;

    pub(crate) fn amice_value_metadata_string(value: LLVMValueRef) -> *const c_char;
}

use inkwell::llvm_sys::prelude::LLVMValueRef;
use std::ffi::c_char;

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_function_fix_stack(function: LLVMValueRef, at_term: i32, max_iterations: i32);

    pub(crate) fn amice_function_verify(function: LLVMValueRef, errmsg: *mut *const c_char) -> i32;

    pub(crate) fn amice_function_is_inline_marked(function: LLVMValueRef) -> bool;

    pub(crate) fn amice_function_clear_stale_analysis_attrs_after_cfg_rewrite(function: LLVMValueRef);
}

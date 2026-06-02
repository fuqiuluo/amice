use inkwell::llvm_sys::prelude::{LLVMModuleRef, LLVMTypeRef, LLVMValueRef};

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    #[cfg(any(
        feature = "llvm12-0",
        feature = "llvm13-0",
        feature = "llvm14-0",
        feature = "llvm15-0",
        feature = "llvm16-0",
        feature = "llvm17-0",
        feature = "llvm18-1",
        feature = "llvm19-1",
        feature = "llvm20-1",
        feature = "llvm21-1",
    ))]
    pub(crate) fn amice_module_append_to_global_ctors(module: LLVMModuleRef, function: LLVMValueRef, priority: i32);

    pub(crate) fn amice_module_append_to_used(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amice_module_append_to_compiler_used(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amice_module_specialize_function(
        original_func: LLVMValueRef,
        module: LLVMModuleRef,
        replacements: *const ArgReplacement,
        replacement_count: u32,
    ) -> LLVMValueRef;

    pub fn amice_module_const_array(element_ty: LLVMTypeRef, values: *mut LLVMValueRef, len: u64) -> LLVMValueRef;
}

#[repr(C)]
pub(crate) struct ArgReplacement {
    pub index: u32,
    pub constant: LLVMValueRef,
}

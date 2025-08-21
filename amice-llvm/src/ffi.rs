use std::ffi::{CStr, c_char, c_void};
use inkwell::llvm_sys::prelude::{LLVMModuleRef, LLVMValueRef};

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
    ))]
    pub(crate) fn amiceAppendToGlobalCtors(module: LLVMModuleRef, function: LLVMValueRef, priority: i32);

    pub(crate) fn amiceAppendToUsed(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amiceAppendToCompilerUsed(module: LLVMModuleRef, value: LLVMValueRef);

    pub(crate) fn amiceFixStack(function: *mut c_void, at_term: i32, max_iterations: i32);

    pub(crate) fn amiceVerifyFunction(function: *mut c_void, errmsg: *mut *const c_char) -> i32;

    pub(crate) fn amiceFreeMsg(errmsg: *const c_char) -> i32;

    pub(crate) fn amiceConstantGetBitCast(value: *mut c_void, ty: *mut c_void) -> *mut c_void;

    pub(crate) fn amiceConstantGetPtrToInt(value: *mut c_void, ty: *mut c_void) -> *mut c_void;

    pub(crate) fn amiceConstantGetIntToPtr(value: *mut c_void, ty: *mut c_void) -> *mut c_void;

    pub(crate) fn amiceConstantGetXor(value1: *mut c_void, value2: *mut c_void) -> *mut c_void;

    pub(crate) fn amiceSplitBasicBlock(
        block: *mut c_void,
        inst: *mut c_void,
        name: *const i8,
        before: i32,
    ) -> *mut c_void;

    pub(crate) fn amiceGetFirstInsertionPt(block: *mut c_void) -> *mut c_void;

    pub(crate) fn amiceGetLLVMVersionMajor() -> i32;

    pub(crate) fn amiceGetLLVMVersionMinor() -> i32;
}

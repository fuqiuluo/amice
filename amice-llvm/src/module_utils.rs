use std::ffi::{c_char, CStr};
use std::result;
use inkwell::llvm_sys::prelude::{LLVMModuleRef, LLVMValueRef};
use inkwell::module::Module;
use inkwell::values::{AsValueRef, FunctionValue, GlobalValue};
use crate::ffi;

pub fn append_to_global_ctors(module: &Module, function: FunctionValue, priority: i32) {
    unsafe {
        ffi::amiceAppendToGlobalCtors(
            module.as_mut_ptr() as LLVMModuleRef,
            function.as_value_ref() as LLVMValueRef,
            priority
        );
    }
}

pub fn append_to_used(module: &Module, value: GlobalValue) {
    unsafe {
        ffi::amiceAppendToUsed(
            module.as_mut_ptr() as LLVMModuleRef,
            value.as_value_ref() as LLVMValueRef,
        );
    }
}

pub fn append_to_compiler_used(module: &Module, value: GlobalValue) {
    unsafe {
        ffi::amiceAppendToCompilerUsed(
            module.as_mut_ptr() as LLVMModuleRef,
            value.as_value_ref() as LLVMValueRef,
        );
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum  VerifyResult {
    Broken(String),
    Ok,
}

pub fn verify_function(function: *mut std::ffi::c_void) -> VerifyResult {
    let mut errmsg: *const c_char = std::ptr::null();
    let broken = unsafe {
        ffi::amiceVerifyFunction(function, &mut errmsg as *mut *const c_char) == 1
    };
    let result = if !errmsg.is_null() && broken {
        let c_errmsg = unsafe { CStr::from_ptr(errmsg) };
        VerifyResult::Broken(c_errmsg.to_string_lossy().into_owned())
    } else {
        VerifyResult::Ok
    };
    unsafe {
        ffi::amiceFreeMsg(errmsg);
    }
    result
}

pub fn verify_function2(function: *mut std::ffi::c_void) -> bool {
    match verify_function(function) {
        VerifyResult::Broken(_) => true,
        VerifyResult::Ok => false
    }
}
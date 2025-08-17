use std::ffi::{c_char, CStr};
use std::result;
use crate::ffi;

pub unsafe fn append_to_global_ctors(module: *mut std::ffi::c_void, function: *mut std::ffi::c_void, priority: i32) {
    ffi::amiceAppendToGlobalCtors(module, function, priority);
}

pub unsafe fn append_to_used(module: *mut std::ffi::c_void, value: *mut std::ffi::c_void) {
    ffi::amiceAppendToUsed(module, value);
}

pub unsafe fn append_to_compiler_used(module: *mut std::ffi::c_void, value: *mut std::ffi::c_void) {
    ffi::amiceAppendToCompilerUsed(module, value);
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
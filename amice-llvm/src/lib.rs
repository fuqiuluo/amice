use std::borrow::Cow;
use std::ffi::{CStr, CString};

mod ffi;
pub mod module_utils;
pub mod ir;

pub fn get_llvm_version_major() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMajor() }
}

pub fn get_llvm_version_minor() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMinor() }
}

pub fn to_c_str(mut s: &str) -> Cow<'_, CStr> {
    if s.is_empty() {
        s = "\0";
    }

    // Start from the end of the string as it's the most likely place to find a null byte
    if !s.chars().rev().any(|ch| ch == '\0') {
        return Cow::from(CString::new(s).expect("unreachable since null bytes are checked"));
    }

    unsafe { Cow::from(CStr::from_ptr(s.as_ptr() as *const _)) }
}

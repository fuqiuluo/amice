use std::borrow::Cow;
use std::ffi::{CStr, CString};

pub mod analysis;
mod ffi;
pub mod ir;
pub mod module_utils;
pub mod annotate;

pub fn get_llvm_version_major() -> i32 {
    unsafe { ffi::amice_get_llvm_version_major() }
}

pub fn get_llvm_version_minor() -> i32 {
    unsafe { ffi::amice_get_llvm_version_minor() }
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

#[cfg(not(any(
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1"
)))]
#[macro_export]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.$ty().ptr_type(AddressSpace::default())
    };
}

#[cfg(any(
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1"
))]
#[macro_export]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.ptr_type(AddressSpace::default())
    };
}

#[cfg(not(any(
    feature = "llvm15-0",
    feature = "llvm16-0",
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1"
)))]
#[macro_export]
macro_rules! build_load {
    ($bd:ident, $ty:expr, $addr:expr, $name:expr) => {
        $bd.build_load($addr, $name)
    };
}

#[cfg(any(
    feature = "llvm15-0",
    feature = "llvm16-0",
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1",
))]
#[macro_export]
macro_rules! build_load {
    ($bd:ident, $ty:expr, $addr:expr, $name:expr) => {
        $bd.build_load($ty, $addr, $name)
    };
}

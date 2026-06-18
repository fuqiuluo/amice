extern crate alloc;

use std::borrow::Cow;
use std::ffi::{CStr, CString};

use inkwell::types::AsTypeRef;
use inkwell::values::{ArrayValue, AsValueRef};

pub mod analysis;
mod annotate;
pub mod code_extractor;
mod ffi;
pub mod inkwell2;

// Public alias kept stable for downstream callers (root `amice` crate).
pub use ffi::module::amice_module_const_array as amice_const_array;

/// Creates a constant array through the C++ shim instead of inkwell's raw C API.
pub fn const_array<'ctx, T, V>(element_ty: T, values: &[V]) -> ArrayValue<'ctx>
where
    T: AsTypeRef,
    V: AsValueRef,
{
    let mut value_refs: Vec<_> = values.iter().map(V::as_value_ref).collect();
    unsafe {
        ArrayValue::new(ffi::module::amice_module_const_array(
            element_ty.as_type_ref(),
            value_refs.as_mut_ptr(),
            value_refs.len() as u64,
        ))
    }
}

pub fn get_llvm_version_major() -> i32 {
    unsafe { ffi::amice_version_major() }
}

pub fn get_llvm_version_minor() -> i32 {
    unsafe { ffi::amice_version_minor() }
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
    feature = "llvm20-1",
    feature = "llvm21-1",
    feature = "llvm22-1",
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
    feature = "llvm20-1",
    feature = "llvm21-1",
    feature = "llvm22-1",
))]
#[macro_export]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.ptr_type(AddressSpace::default())
    };
}

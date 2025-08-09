mod ffi;
pub mod module_utils;
pub mod ir;

pub fn get_llvm_version_major() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMajor() }
}

pub fn get_llvm_version_minor() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMinor() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn test_get_llvm_version_major() {
        assert!(get_llvm_version_major() > 15);

    }
}
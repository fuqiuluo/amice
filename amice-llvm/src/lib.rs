mod ffi;

pub unsafe  fn append_to_global_ctors(
    module: *mut std::ffi::c_void,
    function: *mut std::ffi::c_void,
    priority: i32,
) {
    ffi::amiceAppendToGlobalCtors(module, function, priority);
}

pub fn get_llvm_version_major() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMajor() }
}

pub fn get_llvm_version_minor() -> i32 {
    unsafe { ffi::amiceGetLLVMVersionMinor() }
}


use crate::ffi;

pub unsafe  fn append_to_global_ctors(
    module: *mut std::ffi::c_void,
    function: *mut std::ffi::c_void,
    priority: i32,
) {
    ffi::amiceAppendToGlobalCtors(module, function, priority);
}

pub unsafe fn append_to_used(module: *mut std::ffi::c_void, values: *mut std::ffi::c_void) {
    ffi::amiceAppendToUsed(module, values);
}

pub unsafe fn append_to_compiler_used(module: *mut std::ffi::c_void, values: *mut std::ffi::c_void) {
    ffi::amiceAppendToCompilerUsed(module, values);
}
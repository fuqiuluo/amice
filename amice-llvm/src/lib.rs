mod ffi;

pub unsafe  fn append_to_global_ctors(
    module: *mut std::ffi::c_void,
    function: *mut std::ffi::c_void,
    priority: i32,
) {
    ffi::amiceAppendToGlobalCtors(module, function, priority);
}


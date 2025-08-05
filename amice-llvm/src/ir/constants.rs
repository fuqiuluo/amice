use crate::ffi;

pub unsafe fn get_bitcast_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amiceConstantGetBitCast(value, target_type)
}

pub unsafe fn get_ptr_to_int_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amiceConstantGetPtrToInt(value, target_type)
}

pub unsafe fn get_int_to_ptr_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amiceConstantGetIntToPtr(value, target_type)
}

pub unsafe fn get_xor_constant(
    value1: *mut std::ffi::c_void,
    value2: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amiceConstantGetXor(value1, value2)
}
use crate::ffi;

pub unsafe fn get_bitcast_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amice_constant_get_bit_cast(value, target_type)
}

pub unsafe fn get_ptr_to_int_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amice_constant_get_ptr_to_int(value, target_type)
}

pub unsafe fn get_int_to_ptr_constant(
    value: *mut std::ffi::c_void,
    target_type: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amice_constant_get_int_to_ptr(value, target_type)
}

pub unsafe fn get_xor_constant(
    value1: *mut std::ffi::c_void,
    value2: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    ffi::amice_constant_get_xor(value1, value2)
}
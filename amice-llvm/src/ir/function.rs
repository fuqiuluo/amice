use crate::ffi;

pub unsafe fn fix_stack_at_terminator(function: *mut std::ffi::c_void) {
    ffi::amiceFixStack(function, 1)
}

pub unsafe fn fix_stack(function: *mut std::ffi::c_void) {
    ffi::amiceFixStack(function, 0)
}


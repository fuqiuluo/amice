use crate::ffi;

pub unsafe fn fix_stack(function: *mut std::ffi::c_void) {
    ffi::amiceFixStack(function, 0, 0)
}

pub unsafe fn fix_stack_at_terminator(function: *mut std::ffi::c_void) {
    ffi::amiceFixStack(function, 1, 0)
}

pub unsafe fn fix_stack_with_max_iterations(function: *mut std::ffi::c_void, max_iterations: usize) {
    ffi::amiceFixStack(function, 0, max_iterations as i32)
}
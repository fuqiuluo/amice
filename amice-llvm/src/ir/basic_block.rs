
pub unsafe fn split_basic_block(
    basic_block: *mut std::ffi::c_void,
    inst: *mut std::ffi::c_void,
    new_name: *const std::ffi::c_char,
    before: bool,
) -> *mut std::ffi::c_void {
    crate::ffi::amiceSplitBasicBlock(basic_block, inst, new_name, if before { 1 } else { 0 })
}
/// # Safety
///
/// This function is unsafe because:
/// - It requires valid pointers to LLVM basic block and instruction
/// - The basic_block pointer must point to a valid LLVM BasicBlock object
/// - The inst pointer must point to a valid LLVM Instruction object that exists within the basic block
/// - The new_name pointer must either be null or point to a valid null-terminated C string
/// - The instruction must belong to the specified basic block, otherwise undefined behavior occurs
/// - The caller must ensure the lifetime of the basic block and instruction outlives this operation
///
/// Returns a pointer to the newly created basic block, or null on failure.
pub unsafe fn split_basic_block(
    basic_block: *mut std::ffi::c_void,
    inst: *mut std::ffi::c_void,
    new_name: *const std::ffi::c_char,
    before: bool,
) -> *mut std::ffi::c_void {
    crate::ffi::amiceSplitBasicBlock(basic_block, inst, new_name, if before { 1 } else { 0 })
}

pub unsafe fn get_first_insertion_pt(basic_block: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    crate::ffi::amiceGetFirstInsertionPt(basic_block)
}
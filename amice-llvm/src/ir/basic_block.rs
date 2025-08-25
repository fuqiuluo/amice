use crate::to_c_str;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, InstructionValue};

pub fn split_basic_block<'a>(
    block: BasicBlock<'a>,
    inst: InstructionValue<'a>,
    name: &str,
    before: bool,
) -> Option<BasicBlock<'a>> {
    let c_str_name = to_c_str(name);
    let new_block = unsafe {
        ffi::split_basic_block(
            block.as_mut_ptr() as *mut std::ffi::c_void,
            inst.as_value_ref() as *mut std::ffi::c_void,
            c_str_name.as_ptr(),
            before,
        )
    };
    let value = new_block as LLVMBasicBlockRef;
    unsafe { BasicBlock::new(value) }
}

pub fn get_first_insertion_pt(block: BasicBlock) -> InstructionValue {
    let c_ref = unsafe { ffi::get_first_insertion_pt(block.as_mut_ptr() as *mut std::ffi::c_void) };
    unsafe { InstructionValue::new(c_ref as LLVMValueRef) }
}

pub fn remove_predecessor(block: BasicBlock, pred: BasicBlock) {
    unsafe {
        crate::ffi::amice_basic_block_remove_predecessor(
            block.as_mut_ptr() as LLVMBasicBlockRef,
            block.as_mut_ptr() as LLVMBasicBlockRef,
        )
    }
}

mod ffi {
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
        crate::ffi::amice_split_basic_block(basic_block, inst, new_name, if before { 1 } else { 0 })
    }

    /// # Safety
    ///
    /// This function is unsafe because:
    /// - It requires a valid pointer to an LLVM basic block
    /// - The block pointer must point to a valid LLVM BasicBlock object
    /// - The caller must ensure the basic block remains valid for the duration of the operation
    /// - The basic block must contain at least one instruction, otherwise undefined behavior occurs
    pub unsafe fn get_first_insertion_pt(basic_block: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
        crate::ffi::amice_get_first_insertion_pt(basic_block)
    }
}

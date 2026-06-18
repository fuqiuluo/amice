use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMModuleRef, LLVMValueRef};

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_switch_find_case_dest(inst: LLVMValueRef, b: LLVMBasicBlockRef) -> LLVMValueRef;
    pub(crate) fn amice_switch_get_case_num(inst: LLVMValueRef) -> u32;
    pub(crate) fn amice_switch_get_case_value(inst: LLVMValueRef, index: u32) -> LLVMValueRef;
    pub(crate) fn amice_switch_get_case_dest(inst: LLVMValueRef, index: u32) -> LLVMBasicBlockRef;

    pub(crate) fn amice_gep_accumulate_constant_offset(
        gep: LLVMValueRef,
        module: LLVMModuleRef,
        offset: *mut u64,
    ) -> bool;

    pub(crate) fn amice_phi_replace_incoming_block_with(
        phi_node: LLVMValueRef,
        incoming: LLVMBasicBlockRef,
        new_block: LLVMBasicBlockRef,
    );
}

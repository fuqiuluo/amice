use crate::ffi::{
    amice_basic_block_first_insertion_pt, amice_basic_block_split, amice_phi_replace_incoming_block_with,
    amice_phi_replace_incoming_blocks,
};
use crate::inkwell2::{InstructionExt, LLVMBasicBlockRefExt, LLVMValueRefExt};
use crate::to_c_str;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, InstructionOpcode, InstructionValue};

pub trait BasicBlockExt<'ctx> {
    /// Whether a blockaddress constant refers to this block.
    fn has_address_taken(&self) -> bool;

    /// Whether this block starts an exception-handling pad.
    fn is_eh_pad(&self) -> bool;

    fn split_basic_block(&self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>>;

    fn get_first_insertion_pt(&self) -> InstructionValue<'ctx>;

    #[deprecated(since = "0.1.0", note = "no tested")]
    fn remove_predecessor(&self, pred: BasicBlock<'ctx>);

    fn fix_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>);

    fn fix_phi_node_edges(&self, old_pred: BasicBlock<'ctx>, new_preds: &[BasicBlock<'ctx>]);

    #[deprecated(since = "0.1.0", note = "no tested")]
    fn replace_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>);
}

impl<'ctx> BasicBlockExt<'ctx> for BasicBlock<'ctx> {
    fn has_address_taken(&self) -> bool {
        // SAFETY: The live BasicBlock handle supplies the pointer expected by
        // LLVM's read-only hasAddressTaken query.
        unsafe { crate::ffi::amice_basic_block_has_address_taken(self.as_mut_ptr()) }
    }

    fn is_eh_pad(&self) -> bool {
        // SAFETY: The live BasicBlock handle supplies the pointer expected by
        // LLVM's read-only isEHPad query.
        unsafe { crate::ffi::amice_basic_block_is_eh_pad(self.as_mut_ptr()) }
    }

    fn split_basic_block(&self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>> {
        let c_str_name = to_c_str(name);
        let new_block = unsafe {
            amice_basic_block_split(
                self.as_mut_ptr() as LLVMBasicBlockRef,
                inst.as_value_ref() as LLVMValueRef,
                c_str_name.as_ptr(),
                if before { 1 } else { 0 },
            )
        };
        let value = new_block as LLVMBasicBlockRef;
        value.into_basic_block()
    }

    fn get_first_insertion_pt(&self) -> InstructionValue<'ctx> {
        (unsafe { amice_basic_block_first_insertion_pt(self.as_mut_ptr() as LLVMBasicBlockRef) } as LLVMValueRef)
            .into_instruction_value()
    }

    fn remove_predecessor(&self, pred: BasicBlock<'ctx>) {
        unsafe {
            crate::ffi::amice_basic_block_remove_predecessor(
                self.as_mut_ptr() as LLVMBasicBlockRef,
                pred.as_mut_ptr() as LLVMBasicBlockRef,
            )
        }
    }

    fn fix_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>) {
        for phi in self
            .get_instructions()
            .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        {
            unsafe {
                amice_phi_replace_incoming_block_with(
                    phi.as_value_ref() as LLVMValueRef,
                    old_pred.as_mut_ptr() as LLVMBasicBlockRef,
                    new_pred.as_mut_ptr() as LLVMBasicBlockRef,
                )
            }
        }
    }

    fn fix_phi_node_edges(&self, old_pred: BasicBlock<'ctx>, new_preds: &[BasicBlock<'ctx>]) {
        let new_preds = new_preds
            .iter()
            .map(|block| block.as_mut_ptr() as LLVMBasicBlockRef)
            .collect::<Vec<_>>();
        for phi in self
            .get_instructions()
            .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        {
            unsafe {
                amice_phi_replace_incoming_blocks(
                    phi.as_value_ref() as LLVMValueRef,
                    old_pred.as_mut_ptr() as LLVMBasicBlockRef,
                    new_preds.as_ptr(),
                    u32::try_from(new_preds.len()).expect("Phi gateway list exceeds u32"),
                )
            }
        }
    }

    fn replace_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>) {
        for phi in self
            .get_instructions()
            .take_while(|instruction| instruction.get_opcode() == InstructionOpcode::Phi)
        {
            let phi = phi.into_phi_inst();
            unsafe {
                amice_phi_replace_incoming_block_with(
                    phi.as_value_ref() as LLVMValueRef,
                    old_pred.as_mut_ptr() as LLVMBasicBlockRef,
                    new_pred.as_mut_ptr() as LLVMBasicBlockRef,
                )
            }
        }
    }
}

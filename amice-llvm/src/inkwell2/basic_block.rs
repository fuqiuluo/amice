use crate::ffi::{amice_get_first_insertion_pt, amice_phi_node_replace_incoming_block_with, amice_split_basic_block};
use crate::inkwell2::{InstructionExt, LLVMBasicBlockRefExt, LLVMValueRefExt};
use crate::to_c_str;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::LLVMAddIncoming;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, InstructionOpcode, InstructionValue, PhiValue};

pub trait BasicBlockExt<'ctx> {
    fn split_basic_block(&self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>>;

    fn get_first_insertion_pt(&self) -> InstructionValue<'ctx>;

    #[deprecated(since = "0.1.0", note = "no tested")]
    fn remove_predecessor(&self, pred: BasicBlock<'ctx>);

    fn fix_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>);

    #[deprecated(since = "0.1.0", note = "no tested")]
    fn replace_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>);
}

impl<'ctx> BasicBlockExt<'ctx> for BasicBlock<'ctx> {
    fn split_basic_block(&self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>> {
        let c_str_name = to_c_str(name);
        let new_block = unsafe {
            amice_split_basic_block(
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
        (unsafe { amice_get_first_insertion_pt(self.as_mut_ptr() as LLVMBasicBlockRef) } as LLVMValueRef)
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
        for phi in self.get_first_instruction().iter() {
            if phi.get_opcode() != InstructionOpcode::Phi {
                continue;
            }

            let phi = unsafe { PhiValue::new(phi.as_value_ref()) };
            let incoming_vec = phi
                .get_incomings()
                .filter_map(|(value, pred)| {
                    if pred == old_pred {
                        (value, new_pred).into()
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            let (mut values, mut basic_blocks): (Vec<LLVMValueRef>, Vec<LLVMBasicBlockRef>) = {
                incoming_vec
                    .iter()
                    .map(|&(v, bb)| (v.as_value_ref() as LLVMValueRef, bb.as_mut_ptr() as LLVMBasicBlockRef))
                    .unzip()
            };

            unsafe {
                LLVMAddIncoming(
                    phi.as_value_ref(),
                    values.as_mut_ptr(),
                    basic_blocks.as_mut_ptr(),
                    incoming_vec.len() as u32,
                );
            }
        }
    }

    fn replace_phi_node(&self, old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>) {
        for phi in self.get_first_instruction().iter() {
            if phi.get_opcode() != InstructionOpcode::Phi {
                continue;
            }

            let phi = phi.into_phi_inst();
            unsafe {
                amice_phi_node_replace_incoming_block_with(
                    phi.as_value_ref() as LLVMValueRef,
                    old_pred.as_mut_ptr() as LLVMBasicBlockRef,
                    new_pred.as_mut_ptr() as LLVMBasicBlockRef,
                )
            }
        }
    }
}

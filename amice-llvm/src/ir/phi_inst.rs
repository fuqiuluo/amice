use crate::ffi;
use crate::ffi::amice_phi_node_replace_incoming_block_with;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::LLVMAddIncoming;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, InstructionOpcode, PhiValue};

pub fn update_phi_nodes<'ctx>(old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>, target_block: BasicBlock<'ctx>) {
    for phi in target_block.get_first_instruction().iter() {
        if phi.get_opcode() != InstructionOpcode::Phi {
            break;
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
                .map(|&(v, bb)| (v.as_value_ref(), bb.as_mut_ptr()))
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

pub fn update_phi_nodes2<'ctx>(old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>, target_block: BasicBlock<'ctx>) {
    for phi in target_block.get_first_instruction().iter() {
        if phi.get_opcode() != InstructionOpcode::Phi {
            break;
        }

        let phi = unsafe { PhiValue::new(phi.as_value_ref()) };
        unsafe {
            amice_phi_node_replace_incoming_block_with(
                phi.as_value_ref() as LLVMValueRef,
                old_pred.as_mut_ptr() as LLVMBasicBlockRef,
                new_pred.as_mut_ptr() as LLVMBasicBlockRef,
            )
        }
    }
}

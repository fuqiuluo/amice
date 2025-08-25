use crate::ffi;
use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use inkwell::values::{AsValueRef, BasicValueEnum, InstructionOpcode, InstructionValue};

pub fn get_case_num(inst: InstructionValue) -> u32 {
    assert_eq!(inst.get_opcode(), InstructionOpcode::Switch);

    inst.get_num_operands() / 2 - 1
}

pub fn get_condition(inst: InstructionValue) -> BasicValueEnum {
    inst.get_operand(0).unwrap().left().unwrap()
}

pub fn get_default_block(inst: InstructionValue) -> BasicBlock {
    inst.get_operand(1).unwrap().right().unwrap()
}

pub fn get_cases(inst: InstructionValue) -> Vec<(BasicValueEnum, BasicBlock)> {
    let mut cases = Vec::new();
    for i in (0..get_case_num(inst)).step_by(1) {
        let case_value = inst.get_operand(i * 2 + 2);
        let case_block = inst.get_operand(i * 2 + 3);
        assert!(case_value.is_some());
        assert!(case_block.is_some());
        cases.push((
            case_value.unwrap().left().unwrap(),
            case_block.unwrap().right().unwrap(),
        ));
    }
    cases
}

pub fn find_case_dest<'a>(inst: InstructionValue<'a>, basic_block: BasicBlock) -> Option<BasicValueEnum<'a>> {
    let value_ref = unsafe {
        ffi::amice_switch_find_case_dest(
            inst.as_value_ref() as LLVMValueRef,
            basic_block.as_mut_ptr() as LLVMBasicBlockRef,
        )
    };
    if value_ref.is_null() {
        None
    } else {
        unsafe { Some(BasicValueEnum::new(value_ref)) }
    }
}

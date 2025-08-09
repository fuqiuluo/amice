use llvm_plugin::inkwell::Either;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::values::{BasicValueEnum, InstructionOpcode, InstructionValue};

pub fn get_successor(inst: InstructionValue, idx: u32) -> Option<Either<BasicValueEnum, BasicBlock>> {
    assert_eq!(inst.get_opcode(), InstructionOpcode::Br);

    if inst.get_num_operands() == 1 {
        return inst.get_operand(0);
    }

    assert!(idx < 2);

    inst.get_operand(2 - idx)
}

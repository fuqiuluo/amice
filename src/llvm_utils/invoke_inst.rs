use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::values::{InstructionOpcode, InstructionValue};

/// Get the normal destination basic block from an invoke instruction
pub fn get_normal_destination(inst: InstructionValue) -> Option<BasicBlock> {
    assert_eq!(inst.get_opcode(), InstructionOpcode::Invoke);
    
    let num_operands = inst.get_num_operands();
    
    // For invoke instructions, the operand structure depends on the number of arguments
    // The pattern is: arg1, arg2, ..., argN, normal_dest, unwind_dest, function
    // So normal destination is always the third-to-last operand
    if num_operands >= 3 {
        inst.get_operand(num_operands - 3)?.right()
    } else {
        None
    }
}

/// Get the exception/unwind destination basic block from an invoke instruction  
pub fn get_exception_destination(inst: InstructionValue) -> Option<BasicBlock> {
    assert_eq!(inst.get_opcode(), InstructionOpcode::Invoke);
    
    let num_operands = inst.get_num_operands();
    
    // For invoke instructions, the operand structure depends on the number of arguments
    // The pattern is: arg1, arg2, ..., argN, normal_dest, unwind_dest, function
    // So unwind destination is always the second-to-last operand
    if num_operands >= 2 {
        inst.get_operand(num_operands - 2)?.right()
    } else {
        None
    }
}
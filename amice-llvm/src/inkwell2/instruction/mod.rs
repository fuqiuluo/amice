mod branch_inst;

pub use branch_inst::*;
use inkwell::values::InstructionValue;

pub trait InstructionExt<'ctx> {
    fn into_branch_inst(self) -> BranchInst<'ctx>;
}

impl<'ctx> InstructionExt<'ctx> for InstructionValue<'ctx> {
    fn into_branch_inst(self) -> BranchInst<'ctx> {
        self.into()
    }
}

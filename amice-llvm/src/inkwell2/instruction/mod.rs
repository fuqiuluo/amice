mod branch_inst;
mod gep_inst;
mod phi_inst;
mod switch_inst;

pub use branch_inst::*;
pub use gep_inst::*;
use inkwell::values::InstructionValue;
pub use phi_inst::*;
pub use switch_inst::*;

pub trait InstructionExt<'ctx> {
    fn into_branch_inst(self) -> BranchInst<'ctx>;

    fn into_switch_inst(self) -> SwitchInst<'ctx>;

    fn into_phi_inst(self) -> PhiInst<'ctx>;

    fn into_gep_inst(self) -> GepInst<'ctx>;
}

impl<'ctx> InstructionExt<'ctx> for InstructionValue<'ctx> {
    fn into_branch_inst(self) -> BranchInst<'ctx> {
        self.into()
    }

    fn into_switch_inst(self) -> SwitchInst<'ctx> {
        self.into()
    }

    fn into_phi_inst(self) -> PhiInst<'ctx> {
        self.into()
    }

    fn into_gep_inst(self) -> GepInst<'ctx> {
        self.into()
    }
}

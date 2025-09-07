mod add_inst;
mod alloca_inst;
mod branch_inst;
mod call_inst;
mod gep_inst;
mod load_inst;
mod phi_inst;
mod store_inst;
mod switch_inst;

pub use add_inst::*;
pub use alloca_inst::*;
pub use branch_inst::*;
pub use call_inst::*;
pub use gep_inst::*;
use inkwell::values::InstructionValue;
pub use load_inst::*;
pub use phi_inst::*;
pub use store_inst::*;
pub use switch_inst::*;

pub trait InstructionExt<'ctx> {
    fn into_branch_inst(self) -> BranchInst<'ctx>;

    fn into_switch_inst(self) -> SwitchInst<'ctx>;

    fn into_phi_inst(self) -> PhiInst<'ctx>;

    fn into_gep_inst(self) -> GepInst<'ctx>;

    fn into_call_inst(self) -> CallInst<'ctx>;

    fn into_alloca_inst(self) -> AllocaInst<'ctx>;

    fn into_store_inst(self) -> StoreInst<'ctx>;

    fn into_load_inst(self) -> LoadInst<'ctx>;

    fn into_add_inst(self) -> AddInst<'ctx>;
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

    fn into_call_inst(self) -> CallInst<'ctx> {
        self.into()
    }

    fn into_alloca_inst(self) -> AllocaInst<'ctx> {
        self.into()
    }

    fn into_store_inst(self) -> StoreInst<'ctx> {
        self.into()
    }

    fn into_load_inst(self) -> LoadInst<'ctx> {
        self.into()
    }

    fn into_add_inst(self) -> AddInst<'ctx> {
        self.into()
    }
}

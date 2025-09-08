mod add_inst;
mod alloca_inst;
mod branch_inst;
mod call_inst;
mod gep_inst;
mod load_inst;
mod phi_inst;
mod return_inst;
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
pub use return_inst::*;
pub use store_inst::*;
pub use switch_inst::*;

macro_rules! define_instruction_ext {
    ($(($fn_name:ident, $ty:ident)),+ $(,)?) => {
        pub trait InstructionExt<'ctx> {
            $(fn $fn_name(self) -> $ty<'ctx>;)+
        }

        impl<'ctx> InstructionExt<'ctx> for InstructionValue<'ctx> {
            $(fn $fn_name(self) -> $ty<'ctx> { self.into() })+
        }
    };
}

define_instruction_ext!(
    (into_branch_inst, BranchInst),
    (into_switch_inst, SwitchInst),
    (into_phi_inst, PhiInst),
    (into_gep_inst, GepInst),
    (into_call_inst, CallInst),
    (into_alloca_inst, AllocaInst),
    (into_store_inst, StoreInst),
    (into_load_inst, LoadInst),
    (into_add_inst, AddInst),
    (into_return_inst, ReturnInst),
);

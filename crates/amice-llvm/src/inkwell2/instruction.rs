mod branch_inst;
mod call_inst;
mod gep_inst;
mod phi_inst;
mod switch_inst;

pub use branch_inst::*;
pub use call_inst::*;
pub use gep_inst::*;
use inkwell::values::{AsValueRef, InstructionOpcode, InstructionValue};
pub use phi_inst::*;
pub use switch_inst::*;

/// Whether `opcode` is a branch terminator, conditional or unconditional.
///
/// LLVM 23 splits the single `br` opcode into separate `uncondbr` and
/// `condbr` opcodes, so callers that only ask "is this a branch" must match
/// either variant there.
#[cfg(not(feature = "llvm23-1"))]
pub fn is_branch_opcode(opcode: InstructionOpcode) -> bool {
    opcode == InstructionOpcode::Br
}

/// [`is_branch_opcode`] for LLVM 23, where `br` no longer exists as one opcode.
#[cfg(feature = "llvm23-1")]
pub fn is_branch_opcode(opcode: InstructionOpcode) -> bool {
    matches!(opcode, InstructionOpcode::UncondBr | InstructionOpcode::CondBr)
}

/// The branch opcode used as an internal "this node is a branch" tag.
///
/// Bookkeeping that stores an opcode only to distinguish branch nodes from
/// switch or return nodes uses the unconditional variant on LLVM 23.
#[cfg(not(feature = "llvm23-1"))]
pub fn branch_opcode() -> InstructionOpcode {
    InstructionOpcode::Br
}

/// [`branch_opcode`] for LLVM 23.
#[cfg(feature = "llvm23-1")]
pub fn branch_opcode() -> InstructionOpcode {
    InstructionOpcode::UncondBr
}

pub trait InstructionExt<'ctx> {
    /// Whether instruction flags can introduce poison beyond operand poison.
    fn has_poison_generating_flags(self) -> bool;

    fn into_branch_inst(self) -> BranchInst<'ctx>;

    fn into_switch_inst(self) -> SwitchInst<'ctx>;

    fn into_phi_inst(self) -> PhiInst<'ctx>;

    fn into_gep_inst(self) -> GepInst<'ctx>;

    fn into_call_inst(self) -> CallInst<'ctx>;
}

impl<'ctx> InstructionExt<'ctx> for InstructionValue<'ctx> {
    fn has_poison_generating_flags(self) -> bool {
        // SAFETY: InstructionValue represents a live LLVM instruction.
        unsafe { crate::ffi::amice_instruction_has_poison_generating_flags(self.as_value_ref()) }
    }

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
}

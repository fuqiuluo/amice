use inkwell::basic_block::BasicBlock;
use inkwell::values::InstructionValue;
use crate::ir::basic_block::{get_first_insertion_pt, remove_predecessor, split_basic_block};

pub trait BasicBlockExt<'ctx> {
    fn split_basic_block(self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>>;

    fn get_first_insertion_pt(self) -> InstructionValue<'ctx>;

    #[deprecated(since = "0.1.0", note = "no tested")]
    fn remove_predecessor(self, pred: BasicBlock<'ctx>);
}

impl <'ctx> BasicBlockExt<'ctx> for BasicBlock<'ctx> {
    fn split_basic_block(self, inst: InstructionValue<'ctx>, name: &str, before: bool) -> Option<BasicBlock<'ctx>> {
        split_basic_block(self, inst, name, before)
    }

    fn get_first_insertion_pt(self) -> InstructionValue<'ctx> {
        get_first_insertion_pt(self)
    }

    fn remove_predecessor(self, pred: BasicBlock<'ctx>) {
        remove_predecessor(self, pred)
    }
}

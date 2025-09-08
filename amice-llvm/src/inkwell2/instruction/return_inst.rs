use inkwell::values::{BasicValueEnum, InstructionOpcode, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct ReturnInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> ReturnInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Return);
        Self { inst }
    }

    /// 是否有返回值（void 返回无操作数）
    pub fn has_return_value(&self) -> bool {
        self.get_num_operands() == 1
    }

    /// 获取返回值（若为 void 返回则为 None）
    pub fn get_return_value(&self) -> Option<BasicValueEnum<'ctx>> {
        if self.has_return_value() {
            self.get_operand(0).unwrap().left()
        } else {
            None
        }
    }
}

impl<'ctx> Deref for ReturnInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for ReturnInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for ReturnInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}

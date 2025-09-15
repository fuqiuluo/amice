use inkwell::types::BasicTypeEnum;
use inkwell::values::{InstructionOpcode, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct AllocaInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> AllocaInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Alloca);
        Self { inst }
    }

    pub fn allocated_type(&self) -> BasicTypeEnum<'ctx> {
        self.get_allocated_type().unwrap()
    }
}

impl<'ctx> Deref for AllocaInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for AllocaInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for AllocaInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}

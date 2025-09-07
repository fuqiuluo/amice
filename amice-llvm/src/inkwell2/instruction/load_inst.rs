use inkwell::types::{AnyTypeEnum, BasicTypeEnum};
use inkwell::values::{InstructionOpcode, InstructionValue, PointerValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct LoadInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> LoadInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Load);
        Self { inst }
    }

    pub fn loaded_type(&self) -> AnyTypeEnum<'ctx> {
        self.get_type()
    }

    pub fn get_pointer(&self) -> PointerValue<'ctx> {
        let ptr = self.get_operand(0).unwrap().left().unwrap();
        assert!(ptr.is_pointer_value(), "Expected pointer value, got {:?}", ptr);
        ptr.into_pointer_value()
    }
}

impl<'ctx> Deref for LoadInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for LoadInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for LoadInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}

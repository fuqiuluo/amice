use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, InstructionOpcode, InstructionValue, PointerValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct StoreInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> StoreInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Store);
        Self { inst }
    }

    pub fn get_value(&self) -> BasicValueEnum<'ctx> {
        self.get_operand(0).unwrap().left().unwrap()
    }

    pub fn get_pointer(&self) -> PointerValue<'ctx> {
        let ptr = self.get_operand(1).unwrap().left().unwrap();
        assert!(ptr.is_pointer_value(), "Expected pointer value, got {:?}", ptr);
        ptr.into_pointer_value()
    }
}

impl<'ctx> Deref for StoreInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for StoreInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for StoreInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}

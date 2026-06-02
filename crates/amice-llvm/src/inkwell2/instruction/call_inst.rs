use inkwell::values::{AsValueRef, CallSiteValue, FunctionValue, InstructionOpcode, InstructionValue};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Copy, Clone)]
pub struct CallInst<'ctx> {
    inst: InstructionValue<'ctx>,
}

impl<'ctx> CallInst<'ctx> {
    pub fn new(inst: InstructionValue<'ctx>) -> Self {
        assert_eq!(inst.get_opcode(), InstructionOpcode::Call);
        Self { inst }
    }

    pub fn get_call_function(&self) -> Option<FunctionValue<'ctx>> {
        self.into_call_site_value().get_called_fn_value()
    }

    pub fn into_call_site_value(self) -> CallSiteValue<'ctx> {
        unsafe { CallSiteValue::new(self.inst.as_value_ref()) }
    }
}

impl<'ctx> From<InstructionValue<'ctx>> for CallInst<'ctx> {
    fn from(base: InstructionValue<'ctx>) -> Self {
        Self::new(base)
    }
}

impl<'ctx> Deref for CallInst<'ctx> {
    type Target = InstructionValue<'ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inst
    }
}

impl<'ctx> DerefMut for CallInst<'ctx> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inst
    }
}

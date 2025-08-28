use crate::module_utils::{append_to_compiler_used, append_to_global_ctors, append_to_used};
use inkwell::module::Module;
use inkwell::values::{FunctionValue, GlobalValue};

pub trait ModuleExt {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32);

    fn append_to_used(&mut self, value: GlobalValue);

    fn append_to_compiler_used(&mut self, value: GlobalValue);
}

impl ModuleExt for Module<'_> {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32) {
        append_to_global_ctors(self, function, priority)
    }

    fn append_to_used(&mut self, value: GlobalValue) {
        append_to_used(self, value)
    }

    fn append_to_compiler_used(&mut self, value: GlobalValue) {
        append_to_compiler_used(self, value)
    }
}

use crate::module_utils::{append_to_compiler_used, append_to_global_ctors, append_to_used};
use inkwell::module::Module;
use inkwell::values::{FunctionValue, GlobalValue};
use crate::annotate::read_function_annotate;

pub trait ModuleExt<'ctx> {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32);

    fn append_to_used(&mut self, value: GlobalValue);

    fn append_to_compiler_used(&mut self, value: GlobalValue);

    fn read_function_annotate(&mut self, func: FunctionValue<'ctx>) -> Result<Vec<String>, &'static str>;
}

impl<'ctx> ModuleExt<'ctx> for Module<'ctx> {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32) {
        append_to_global_ctors(self, function, priority)
    }

    fn append_to_used(&mut self, value: GlobalValue) {
        append_to_used(self, value)
    }

    fn append_to_compiler_used(&mut self, value: GlobalValue) {
        append_to_compiler_used(self, value)
    }

    fn read_function_annotate(&mut self, func: FunctionValue<'ctx>) -> Result<Vec<String>, &'static str> {
        read_function_annotate(self, func)
    }
}

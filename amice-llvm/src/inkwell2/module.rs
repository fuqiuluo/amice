use crate::annotate::read_function_annotate;
use crate::ffi;
use crate::ffi::ArgReplacement;
use crate::inkwell2::LLVMValueRefExt;
use inkwell::llvm_sys::prelude::{LLVMModuleRef, LLVMValueRef};
use inkwell::module::Module;
use inkwell::values::{AsValueRef, FunctionValue, GlobalValue};

pub trait ModuleExt<'ctx> {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32);

    fn append_to_used(&mut self, value: GlobalValue);

    fn append_to_compiler_used(&mut self, value: GlobalValue);

    fn read_function_annotate(&mut self, func: FunctionValue<'ctx>) -> Result<Vec<String>, &'static str>;

    #[cfg(not(feature = "android-ndk"))]
    unsafe fn specialize_function_by_args(
        &self,
        original_func: FunctionValue<'ctx>,
        args: &[(u32, LLVMValueRef)],
    ) -> Result<FunctionValue<'ctx>, &'static str>;
}

impl<'ctx> ModuleExt<'ctx> for Module<'ctx> {
    fn append_to_global_ctors(&mut self, function: FunctionValue, priority: i32) {
        unsafe {
            ffi::amice_append_to_global_ctors(
                self.as_mut_ptr() as LLVMModuleRef,
                function.as_value_ref() as LLVMValueRef,
                priority,
            );
        }
    }

    fn append_to_used(&mut self, value: GlobalValue) {
        unsafe {
            ffi::amice_append_to_used(self.as_mut_ptr() as LLVMModuleRef, value.as_value_ref() as LLVMValueRef);
        }
    }

    fn append_to_compiler_used(&mut self, value: GlobalValue) {
        unsafe {
            ffi::amice_append_to_compiler_used(
                self.as_mut_ptr() as LLVMModuleRef,
                value.as_value_ref() as LLVMValueRef,
            );
        }
    }

    fn read_function_annotate(&mut self, func: FunctionValue<'ctx>) -> Result<Vec<String>, &'static str> {
        read_function_annotate(self, func)
    }

    unsafe fn specialize_function_by_args(
        &self,
        original_func: FunctionValue<'ctx>,
        args: &[(u32, LLVMValueRef)],
    ) -> Result<FunctionValue<'ctx>, &'static str> {
        if original_func.is_null() {
            return Err("Null pointer");
        }

        let arg_replacements: Vec<ArgReplacement> = args
            .iter()
            .map(|(index, constant)| ArgReplacement {
                index: *index,
                constant: *constant,
            })
            .collect();

        let result = unsafe {
            ffi::amice_specialize_function(
                original_func.as_value_ref() as LLVMValueRef,
                self.as_mut_ptr() as LLVMModuleRef,
                arg_replacements.as_ptr(),
                arg_replacements.len() as u32,
            )
        };

        if result.is_null() {
            Err("Specialization failed")
        } else {
            result
                .into_function_value()
                .ok_or("Specialization failed: invalid function value")
        }
    }
}

use crate::module_utils::{verify_function, verify_function2};
use inkwell::values::FunctionValue;
pub use crate::module_utils::VerifyResult;

pub trait FunctionExt {
    fn verify_function(self) -> VerifyResult;

    fn verify_function_bool(self) -> bool;
}

impl FunctionExt for FunctionValue<'_> {
    fn verify_function(self) -> VerifyResult {
        verify_function(self)
    }

    fn verify_function_bool(self) -> bool {
        verify_function2(self)
    }
}

use crate::ffi;
use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, PointerValue};

pub trait PointerValueExt<'ctx> {
    fn replace_non_metadata_uses_with(self, replacement: PointerValue<'ctx>);

    fn drop_droppable_uses(self);

    fn has_undroppable_uses(self) -> bool;
}

impl<'ctx> PointerValueExt<'ctx> for PointerValue<'ctx> {
    fn replace_non_metadata_uses_with(self, replacement: PointerValue<'ctx>) {
        unsafe {
            ffi::amice_value_replace_non_metadata_uses_with(
                self.as_value_ref() as LLVMValueRef,
                replacement.as_value_ref() as LLVMValueRef,
            );
        }
    }

    fn drop_droppable_uses(self) {
        unsafe {
            ffi::amice_value_drop_droppable_uses(self.as_value_ref() as LLVMValueRef);
        }
    }

    fn has_undroppable_uses(self) -> bool {
        unsafe { ffi::amice_value_has_undroppable_uses(self.as_value_ref() as LLVMValueRef) }
    }
}

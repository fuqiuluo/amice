use crate::ffi;
use std::ffi::CStr;

use inkwell::llvm_sys::prelude::LLVMValueRef;
use inkwell::values::{AsValueRef, PointerValue};

pub trait PointerValueExt<'ctx> {
    fn replace_non_metadata_uses_with(self, replacement: PointerValue<'ctx>);

    fn drop_droppable_uses(self);

    fn has_undroppable_uses(self) -> bool;
}

pub trait ValueExt {
    fn metadata_string(self) -> Option<String>;
}

pub fn metadata_string_from_value_ref(value: LLVMValueRef) -> Option<String> {
    unsafe {
        let ptr = ffi::amice_value_metadata_string(value);
        if ptr.is_null() {
            return None;
        }
        let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        ffi::amice_free_string(ptr);
        Some(text)
    }
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

impl<T> ValueExt for T
where
    T: AsValueRef,
{
    fn metadata_string(self) -> Option<String> {
        metadata_string_from_value_ref(self.as_value_ref() as LLVMValueRef)
    }
}

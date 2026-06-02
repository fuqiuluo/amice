use std::ffi::c_char;

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_attribute_enum_kind_to_str(kind: u32) -> *const c_char;
}

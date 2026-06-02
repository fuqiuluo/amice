use std::ffi::c_char;

#[link(name = "amice-llvm-ffi")]
unsafe extern "C" {
    pub(crate) fn amice_version_major() -> i32;

    pub(crate) fn amice_version_minor() -> i32;

    pub(crate) fn amice_free_string(errmsg: *const c_char) -> i32;
}

pub mod bogus_control_flow;
pub mod clone_function;
pub mod flatten;
pub mod function_wrapper;
pub mod indirect_branch;
pub mod indirect_call;
pub mod lower_switch;
pub mod mba;
pub mod shuffle_blocks;
pub mod split_basic_block;
pub mod string_encryption;
pub mod vm_flatten;

#[cfg(any(feature = "llvm15-0", feature = "llvm16-0",))]
#[macro_export]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.$ty().ptr_type(AddressSpace::default())
    };
}

#[cfg(any(
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1"
))]
#[macro_export]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.ptr_type(AddressSpace::default())
    };
}

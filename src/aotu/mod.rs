pub mod indirect_branch;
pub mod string_encryption;

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

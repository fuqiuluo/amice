use inkwell::builder::{Builder, BuilderError};
use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

pub trait BuilderExt<'ctx> {
    fn build_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError>;

    fn build_load2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, BuilderError>;

    fn build_in_bounds_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError>;

    fn build_struct_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        index: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError>;
}

impl<'ctx> BuilderExt<'ctx> for Builder<'ctx> {
    fn build_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        #[cfg(not(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        )))]
        return unsafe { self.build_gep(ptr, ordered_indexes, name) };

        #[cfg(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        ))]
        return unsafe { self.build_gep(pointee_ty, ptr, ordered_indexes, name) };

        panic!("Unsupported LLVM version");
    }

    fn build_load2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, BuilderError> {
        #[cfg(not(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        )))]
        return self.build_load(ptr, name);

        #[cfg(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        ))]
        return self.build_load(pointee_ty, ptr, name);

        panic!("Unsupported LLVM version");
    }

    fn build_in_bounds_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        ordered_indexes: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        #[cfg(not(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        )))]
        return unsafe { self.build_in_bounds_gep(ptr, ordered_indexes, name) };

        #[cfg(any(
            feature = "llvm15-0",
            feature = "llvm16-0",
            feature = "llvm17-0",
            feature = "llvm18-1",
            feature = "llvm19-1",
            feature = "llvm20-1",
        ))]
        return unsafe { self.build_in_bounds_gep(pointee_ty, ptr, ordered_indexes, name) };

        panic!("Unsupported LLVM version");
    }

    fn build_struct_gep2<T: BasicType<'ctx>>(
        &self,
        pointee_ty: T,
        ptr: PointerValue<'ctx>,
        index: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, BuilderError> {
        #[cfg(any(
            feature = "llvm11-0",
            feature = "llvm12-0",
            feature = "llvm13-0",
            feature = "llvm14-0"
        ))]
        return self.build_struct_gep(ptr, index, name);

        #[cfg(not(any(
            feature = "llvm11-0",
            feature = "llvm12-0",
            feature = "llvm13-0",
            feature = "llvm14-0"
        )))]
        return self.build_struct_gep(pointee_ty, ptr, index, name);

        panic!("Unsupported LLVM version");
    }
}
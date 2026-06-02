//! Raw FFI declarations for the amice-llvm C++ shims (the `libamice-llvm-ffi`
//! static archive built from `cpp/`).
//!
//! One submodule per LLVM domain, mirroring the C++ translation units. Every
//! symbol follows the `amice_<domain>_<operation>` scheme. The safe wrappers
//! live one layer up (`inkwell2`, `analysis`, `code_extractor`); call those,
//! not these.
//!
//! The submodule symbols are re-exported flat here, so callers use
//! `crate::ffi::<name>` regardless of which domain a symbol belongs to.

pub(crate) mod attribute;
pub(crate) mod basic_block;
pub(crate) mod code_extractor;
pub(crate) mod dominators;
pub(crate) mod function;
pub(crate) mod instruction;
pub(crate) mod module;
pub(crate) mod support;

pub(crate) use attribute::*;
pub(crate) use basic_block::*;
pub(crate) use code_extractor::*;
pub(crate) use dominators::*;
pub(crate) use function::*;
pub(crate) use instruction::*;
pub(crate) use module::*;
pub(crate) use support::*;

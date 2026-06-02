#pragma once
//
// Shared conventions for the amice-llvm C++ FFI shims.
//
// These shims fill gaps that inkwell / llvm-sys don't expose. They are grouped
// by LLVM domain, one translation unit per domain (module.cc, function.cc,
// basic_block.cc, instruction.cc, dominators.cc, code_extractor.cc,
// attribute.cc, support.cc). Every exported symbol follows the naming scheme
//
//     amice_<domain>_<operation>
//
// and the Rust side declares the matching extern in src/ffi/<domain>.rs.
//
// ABI conventions (keep these invariant across every amice_* export):
//
//  * IR handles cross the boundary as the matching llvm::* pointer/reference.
//    The Rust extern uses the corresponding LLVM-C ref type (LLVMValueRef,
//    LLVMModuleRef, LLVMBasicBlockRef, ...). LLVM's wrap/unwrap is an identity
//    reinterpret_cast, so e.g. `llvm::Function*` and `LLVMValueRef` are
//    layout-compatible; pass the concrete `llvm::X*`/`llvm::X&` here and let the
//    Rust declaration name the ref type.
//
//  * Any `char*` returned to Rust is heap-allocated with malloc and must be
//    released by Rust through `amice_free_string`.
//
//  * Opaque C++ objects we own (llvm::DominatorTree, llvm::CodeExtractor) are
//    handed to Rust as raw pointers behind an opaque ref type; Rust must call
//    the matching `*_destroy` / `*_delete` to release them.
//
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include <llvm/IR/BasicBlock.h>
#include <llvm/IR/Constants.h>
#include <llvm/IR/Function.h>
#include <llvm/IR/Instructions.h>
#include <llvm/IR/Module.h>
#include <llvm-c/Core.h>

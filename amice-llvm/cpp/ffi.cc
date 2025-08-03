#include <cstdint>
#include <memory>
#include <mutex>
#include <utility>

#include <llvm/ADT/ArrayRef.h>
#include <llvm/IR/Function.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/PassManager.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Passes/PassPlugin.h>
#include "llvm/Transforms/Utils/ModuleUtils.h"

#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 14)
#include <llvm/Passes/OptimizationLevel.h>
using LlvmOptLevel = llvm::OptimizationLevel;
#else
using LlvmOptLevel = llvm::PassBuilder::OptimizationLevel;
#endif

// copy from https://github.com/jamesmth/llvm-plugin-rs/blob/feat%2Fllvm-20/llvm-plugin/cpp/ffi.cc
extern "C" {
void amiceAppendToGlobalCtors(llvm::Module &M, llvm::Function *F, int P) {
     llvm::appendToGlobalCtors(M, F, P);
}

int amiceGetLLVMVersionMajor() {
  return LLVM_VERSION_MAJOR;
}

int amiceGetLLVMVersionMinor() {
  return LLVM_VERSION_MINOR;
}
}
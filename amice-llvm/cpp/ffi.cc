// copy from https://github.com/jamesmth/llvm-plugin-rs/blob/feat%2Fllvm-20/llvm-plugin/cpp/ffi.cc

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
#include "llvm/IR/BasicBlock.h"
#include "llvm/Transforms/Utils/ModuleUtils.h"
#include "llvm/IR/Constants.h"
#include "llvm/IR/AbstractCallSite.h"
#include "llvm/IR/InstrTypes.h"

#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 14)
#include <llvm/Passes/OptimizationLevel.h>
using LlvmOptLevel = llvm::OptimizationLevel;
#else
using LlvmOptLevel = llvm::PassBuilder::OptimizationLevel;
#endif

extern "C" {
int amiceGetLLVMVersionMajor() {
  return LLVM_VERSION_MAJOR;
}

int amiceGetLLVMVersionMinor() {
  return LLVM_VERSION_MINOR;
}

void amiceAppendToGlobalCtors(llvm::Module &M, llvm::Function *F, int P) {
     llvm::appendToGlobalCtors(M, F, P);
}

void amiceAppendToUsed(llvm::Module &M, llvm::GlobalValue * V) {
    llvm::appendToUsed(M, {V});
}

void amiceAppendToCompilerUsed(llvm::Module &M, llvm::GlobalValue * V) {
    llvm::appendToCompilerUsed(M, {V});
}

llvm::Constant * amiceConstantGetBitCast(llvm::Constant *C, llvm::Type *Ty) {
    return llvm::ConstantExpr::getBitCast(C, Ty);
}

llvm::Constant * amiceConstantGetPtrToInt(llvm::Constant *C, llvm::Type *Ty) {
    return llvm::ConstantExpr::getPtrToInt(C, Ty);
}

llvm::Constant * amiceConstantGetIntToPtr(llvm::Constant *C, llvm::Type *Ty) {
    return llvm::ConstantExpr::getIntToPtr(C, Ty);
}

llvm::Constant * amiceConstantGetXor(llvm::Constant *C1, llvm::Constant *C2) {
    return llvm::ConstantExpr::getXor(C1, C2);
}

llvm::BasicBlock * 	amiceSplitBasicBlock (llvm::BasicBlock * BB, llvm::Instruction *I, char* N, int B) {
    return BB->splitBasicBlock(I, N, B);
}

}
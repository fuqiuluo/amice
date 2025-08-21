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
#include "llvm/Transforms/Utils/Local.h"
#include "llvm/Support/Casting.h"
#include "llvm/Pass.h"
#include "llvm/IR/CFG.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/ADT/SmallSet.h"
#include "llvm/ADT/SmallVector.h"
#include "llvm/Support/raw_ostream.h"
#include "llvm/LinkAllPasses.h"
#include "llvm/Transforms/Utils/Cloning.h"
#include "llvm/IR/Verifier.h"
#include "llvm/IR/Instructions.h"
#include "llvm/ADT/Statistic.h"
#include "llvm/Analysis/LoopInfo.h"
#include "llvm/IR/Dominators.h"
#include "llvm/IR/InstIterator.h"
#include "llvm/Transforms/Scalar.h"
#include "llvm/Transforms/Utils.h"
#include "llvm/Transforms/Utils/BasicBlockUtils.h"

extern "C" {

void amice_append_to_global_ctors(llvm::Module &M, llvm::Function *F, int P) {
     llvm::appendToGlobalCtors(M, F, P);
}

void amice_append_to_used(llvm::Module &M, llvm::GlobalValue * V) {
    llvm::appendToUsed(M, {V});
}

void amice_append_to_compiler_used(llvm::Module &M, llvm::GlobalValue * V) {
    llvm::appendToCompilerUsed(M, {V});
}

llvm::BasicBlock * 	amice_split_basic_block (llvm::BasicBlock * BB, llvm::Instruction *I, char* N, int B) {
    return BB->splitBasicBlock(I, N, B);
}

llvm::Instruction* amice_get_first_insertion_pt(llvm::BasicBlock* bb) {
    return llvm::cast<llvm::Instruction>(bb->getFirstInsertionPt());
}

void amice_basic_block_remove_predecessor(llvm::BasicBlock* B, llvm::BasicBlock* P) {
    B->removePredecessor(P);
}

}
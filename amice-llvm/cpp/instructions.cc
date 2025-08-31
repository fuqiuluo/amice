#include <llvm/ADT/ArrayRef.h>
#include <llvm/IR/Function.h>
#include <llvm/IR/Module.h>
#include "llvm/IR/BasicBlock.h"
#include "llvm/Transforms/Utils/ModuleUtils.h"
#include "llvm/IR/Constants.h"
#include "llvm/IR/AbstractCallSite.h"
#include "llvm/IR/InstrTypes.h"
#include "llvm/IR/Attributes.h"
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
#include "llvm/ADT/APInt.h"
#include "llvm/IR/Module.h"

extern "C" {

llvm::ConstantInt* amice_switch_find_case_dest(llvm::SwitchInst* S, llvm::BasicBlock* B) {
    return S->findCaseDest (B);
}

bool amice_is_inline_marked_function(llvm::Function &F) {
    if (F.hasFnAttribute(llvm::Attribute::AlwaysInline)) {
        return true;
    }

    if (F.hasFnAttribute(llvm::Attribute::InlineHint)) {
        return true;
    }

    return false;
}

bool amice_gep_accumulate_constant_offset(llvm::Instruction *I, llvm::Module *M, uint64_t *OutOffset) {
    if (auto *GEP = llvm::dyn_cast<llvm::GetElementPtrInst>(I)) {
        const llvm::DataLayout &DL = M->getDataLayout();
        llvm::APInt OffsetAI(DL.getIndexSizeInBits(/*AS=*/0), 0);
        bool result = GEP->accumulateConstantOffset(DL, OffsetAI);
        uint64_t Offset = OffsetAI.getZExtValue();
        *OutOffset = Offset;
        return result;
    }
    return false;
}

}
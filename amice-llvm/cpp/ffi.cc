// copy from https://github.com/jamesmth/llvm-plugin-rs/blob/feat%2Fllvm-20/llvm-plugin/cpp/ffi.cc

#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <utility>
#include <vector>
#include <string>
#include <optional>

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

bool valueEscapes(llvm::Instruction *Inst) {
  if (!Inst->getType()->isSized())
    return false;

  llvm::BasicBlock *BB = Inst->getParent();
  for (llvm::Value::use_iterator UI = Inst->use_begin(), E = Inst->use_end(); UI != E;
       ++UI) {
    llvm::Instruction *I = llvm::cast<llvm::Instruction>(*UI);
    if (I->getParent() != BB || llvm::isa<llvm::PHINode>(I)) {
      return true;
    }
  }
  return false;
}

static bool valueEscapesOfficial(const llvm::Instruction &Inst) {
  if (!Inst.getType()->isSized())
    return false;

  const llvm::BasicBlock *BB = Inst.getParent();
  for (const llvm::User *U : Inst.users()) {
    const llvm::Instruction *UI = llvm::cast<llvm::Instruction>(U);
    if (UI->getParent() != BB || llvm::isa<llvm::PHINode>(UI))
      return true;
  }
  return false;
}

void amiceFixStack(llvm::Function *f, int AtTerminator, int MaxIterations) {
    // https://bbs.kanxue.com/thread-268789-1.htm
    std::vector<llvm::PHINode *> tmpPhi;
    std::vector<llvm::Instruction *> tmpReg;
    llvm::BasicBlock *bbEntry = &*f->begin();

    auto isDemotableValueTy = [](llvm::Type *Ty) -> bool {
        if (!Ty) return false;
        if (Ty->isVoidTy()) return false;
        if (Ty->isTokenTy()) return false;
        return Ty->isFirstClassType();
    };

    int iteration = 0;
    do {
        tmpPhi.clear();
        tmpReg.clear();
        for (llvm::Function::iterator i = f->begin(); i != f->end(); i++) {
            for (llvm::BasicBlock::iterator j = i->begin(); j != i->end(); j++) {
                if (llvm::isa<llvm::PHINode>(j)){
                    llvm::PHINode *phi = llvm::cast<llvm::PHINode>(j);
                    tmpPhi.push_back(phi);
                    continue;
                }

                // 跳过 terminator（包括 invoke/switch/ret/br/callbr 等）
                if (j->isTerminator())
                    continue;

//                // 跳过 EH pad / landingpad
//                if (llvm::isa<llvm::LandingPadInst>(j) ||
//                    llvm::isa<llvm::CatchPadInst>(j) ||
//                    llvm::isa<llvm::CleanupPadInst>(j))
//                    continue;

//                if (!isDemotableValueTy(j->getType()))
//                    continue;


                if (!(llvm::isa<llvm::AllocaInst>(j) && j->getParent() == bbEntry) &&
                    (valueEscapes(&*j) || j->isUsedOutsideOfBlock(&*i))) {
                    tmpReg.push_back(&*j);
                    continue;
                }
            }
        }
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        for (unsigned int i = 0; i < tmpReg.size(); i++){
            if(AtTerminator) {
                llvm::DemoteRegToStack(*tmpReg.at(i), false, std::optional<llvm::BasicBlock::iterator>{bbEntry->getTerminator()});
            } else {
                llvm::DemoteRegToStack(*tmpReg.at(i));
            }
        }
        for (unsigned int i = 0; i < tmpPhi.size(); i++){
            if(AtTerminator) {
                llvm::DemotePHIToStack(tmpPhi.at(i), std::optional<llvm::BasicBlock::iterator>{bbEntry->getTerminator()});
            } else {
                llvm::DemotePHIToStack(tmpPhi.at(i));
            }
        }
#else
        for (unsigned int i = 0; i < tmpReg.size(); i++){
            if(AtTerminator) {
                llvm::DemoteRegToStack(*tmpReg.at(i), false, bbEntry->getTerminator());
            } else {
                llvm::DemoteRegToStack(*tmpReg.at(i));
            }
        }
        for (unsigned int i = 0; i < tmpPhi.size(); i++){
            if(AtTerminator) {
                llvm::DemotePHIToStack(tmpPhi.at(i), bbEntry->getTerminator());
            } else {
                llvm::DemotePHIToStack(tmpPhi.at(i));
            }
        }
#endif
         iteration++;
         if(MaxIterations != 0 && iteration > MaxIterations) {
            break;
         }
    } while (tmpReg.size() != 0 || tmpPhi.size() != 0);
}

int amiceFreeMsg(char* err) {
    if(err) {
        free(err);
        return 0;
    }
    return -1;
}

}
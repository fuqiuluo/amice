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
#include "llvm/IR/Attributes.h"

#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 14)
#include <llvm/Passes/OptimizationLevel.h>
using LlvmOptLevel = llvm::OptimizationLevel;
#else
using LlvmOptLevel = llvm::PassBuilder::OptimizationLevel;
#endif

static bool valueEscapes(llvm::Instruction *Inst) {
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

extern "C" {
int amice_get_llvm_version_major() {
  return LLVM_VERSION_MAJOR;
}

int amice_get_llvm_version_minor() {
  return LLVM_VERSION_MINOR;
}

void amice_fix_stack(llvm::Function *f, int AtTerminator, int MaxIterations) {
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
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19)
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

char* amice_attribute_enum_kind_to_str(llvm::Attribute::AttrKind kind) {
#define ENUM_CASE(name, simple_name) case name: str = #simple_name; break;
    std::string str;
    switch (kind) {
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::AllocAlign, AllocAlign) // llvm 15
        ENUM_CASE(llvm::Attribute::AllocatedPointer, AllocatedPointer) // llvm 15
#endif
        ENUM_CASE(llvm::Attribute::AlwaysInline, AlwaysInline)
        ENUM_CASE(llvm::Attribute::Builtin, Builtin)
        ENUM_CASE(llvm::Attribute::NoUndef, NoUndef)
        ENUM_CASE(llvm::Attribute::Cold, Cold)
        ENUM_CASE(llvm::Attribute::Convergent, Convergent)
        ENUM_CASE(llvm::Attribute::Hot, Hot)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 14)
        ENUM_CASE(llvm::Attribute::DisableSanitizerInstrumentation, DisableSanitizerInstrumentation) // llvm 14
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::FnRetThunkExtern, FnRetThunkExtern) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::HybridPatchable, HybridPatchable) // llvm 19
#endif
        ENUM_CASE(llvm::Attribute::InlineHint, InlineHint)
        ENUM_CASE(llvm::Attribute::InReg, InReg)
        ENUM_CASE(llvm::Attribute::JumpTable, JumpTable)
        ENUM_CASE(llvm::Attribute::MinSize, MinSize)
        ENUM_CASE(llvm::Attribute::Naked, Naked)
        ENUM_CASE(llvm::Attribute::Nest, Nest)
        ENUM_CASE(llvm::Attribute::NoAlias, NoAlias)
        ENUM_CASE(llvm::Attribute::NoBuiltin, NoBuiltin)
        ENUM_CASE(llvm::Attribute::NoCallback, NoCallback)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::NoDivergenceSource, NoDivergenceSource) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::NoDuplicate, NoDuplicate)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::NoExt, NoExt) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::NoFree, NoFree)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::DeadOnUnwind, DeadOnUnwind) // llvm 18
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 21) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::DeadOnReturn, DeadOnReturn)
#endif
        ENUM_CASE(llvm::Attribute::NoImplicitFloat, NoImplicitFloat)
        ENUM_CASE(llvm::Attribute::NoInline, NoInline)
        ENUM_CASE(llvm::Attribute::NonLazyBind, NonLazyBind)
        ENUM_CASE(llvm::Attribute::NoMerge, NoMerge)
        ENUM_CASE(llvm::Attribute::NonNull, NonNull)
        ENUM_CASE(llvm::Attribute::NoRecurse, NoRecurse)
        ENUM_CASE(llvm::Attribute::NoRedZone, NoRedZone)
        ENUM_CASE(llvm::Attribute::NoReturn, NoReturn)
        ENUM_CASE(llvm::Attribute::NoSync, NoSync)
        ENUM_CASE(llvm::Attribute::NoCfCheck, NoCfCheck)
        ENUM_CASE(llvm::Attribute::NoProfile, NoProfile)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 16)
        ENUM_CASE(llvm::Attribute::SkipProfile, SkipProfile) // llvm 16
#endif
        ENUM_CASE(llvm::Attribute::NoUnwind, NoUnwind)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::NoSanitizeBounds, NoSanitizeBounds) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
        ENUM_CASE(llvm::Attribute::NoSanitizeCoverage, NoSanitizeCoverage) // llvm 13
#endif
        ENUM_CASE(llvm::Attribute::NullPointerIsValid, NullPointerIsValid)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::OptimizeForDebugging, OptimizeForDebugging) // llvm18
#endif
        ENUM_CASE(llvm::Attribute::OptForFuzzing, OptForFuzzing)
        ENUM_CASE(llvm::Attribute::OptimizeForSize, OptimizeForSize)
        ENUM_CASE(llvm::Attribute::OptimizeNone, OptimizeNone)
        ENUM_CASE(llvm::Attribute::ReadNone, ReadNone)
        ENUM_CASE(llvm::Attribute::ReadOnly, ReadOnly)
        ENUM_CASE(llvm::Attribute::Returned, Returned)
        ENUM_CASE(llvm::Attribute::ImmArg, ImmArg)
        ENUM_CASE(llvm::Attribute::ReturnsTwice, ReturnsTwice)
        ENUM_CASE(llvm::Attribute::SafeStack, SafeStack)
        ENUM_CASE(llvm::Attribute::ShadowCallStack, ShadowCallStack)
        ENUM_CASE(llvm::Attribute::SExt, SExt)
        ENUM_CASE(llvm::Attribute::Speculatable, Speculatable)
        ENUM_CASE(llvm::Attribute::StackProtect, StackProtect)
        ENUM_CASE(llvm::Attribute::StackProtectReq, StackProtectReq)
        ENUM_CASE(llvm::Attribute::StackProtectStrong, StackProtectStrong)
        ENUM_CASE(llvm::Attribute::StrictFP, StrictFP)
        ENUM_CASE(llvm::Attribute::SanitizeAddress, SanitizeAddress)
        ENUM_CASE(llvm::Attribute::SanitizeThread, SanitizeThread)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::SanitizeType, SanitizeType) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::SanitizeMemory, SanitizeMemory)
        ENUM_CASE(llvm::Attribute::SanitizeHWAddress, SanitizeHWAddress)
        ENUM_CASE(llvm::Attribute::SanitizeMemTag, SanitizeMemTag)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19) && (LLVM_VERSION_MINOR >= 1)
        ENUM_CASE(llvm::Attribute::SanitizeNumericalStability, SanitizeNumericalStability) // llvm 19
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::SanitizeRealtime, SanitizeRealtime) // llvm 20
        ENUM_CASE(llvm::Attribute::SanitizeRealtimeBlocking, SanitizeRealtimeBlocking) // llvm 20
#endif
        ENUM_CASE(llvm::Attribute::SpeculativeLoadHardening, SpeculativeLoadHardening)
        ENUM_CASE(llvm::Attribute::SwiftError, SwiftError)
        ENUM_CASE(llvm::Attribute::SwiftSelf, SwiftSelf)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
        ENUM_CASE(llvm::Attribute::SwiftAsync, SwiftAsync) // llvm 13
#endif
        ENUM_CASE(llvm::Attribute::WillReturn, WillReturn)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::Writable, Writable) // llvm18
#endif
        ENUM_CASE(llvm::Attribute::WriteOnly, WriteOnly)
        ENUM_CASE(llvm::Attribute::ZExt, ZExt)
        ENUM_CASE(llvm::Attribute::MustProgress, MustProgress)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 15)
        ENUM_CASE(llvm::Attribute::PresplitCoroutine, PresplitCoroutine) // llvm 15
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 18)
        ENUM_CASE(llvm::Attribute::CoroDestroyOnlyWhenComplete, CoroDestroyOnlyWhenComplete) // llvm18
#endif
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 20)
        ENUM_CASE(llvm::Attribute::CoroElideSafe, CoroElideSafe) // llvm 20
#endif
        default: str = "unknown"; break;
    }
    char* cstr = (char*)malloc(str.size() + 1);
    strcpy(cstr, str.c_str());
    cstr[str.size()] = '\0';
    return cstr;
}

int amice_free_msg(char* err) {
    if(err) {
        free(err);
        return 0;
    }
    return -1;
}

}
// Function-level helpers: register/phi stack demotion, IR verification, and
// inline-attribute inspection.
#include "amice_ffi.h"

#include <optional>

#include <llvm/IR/Attributes.h>
#include <llvm/IR/Verifier.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Transforms/Utils/Local.h>

namespace {

bool valueEscapes(llvm::Instruction *Inst) {
    if (!Inst->getType()->isSized())
        return false;

    llvm::BasicBlock *BB = Inst->getParent();
    for (llvm::Value::use_iterator UI = Inst->use_begin(), E = Inst->use_end(); UI != E; ++UI) {
        llvm::Instruction *I = llvm::cast<llvm::Instruction>(*UI);
        if (I->getParent() != BB || llvm::isa<llvm::PHINode>(I)) {
            return true;
        }
    }
    return false;
}

bool isDemotableValueTy(llvm::Type *Ty) {
    if (!Ty)
        return false;
    if (Ty->isVoidTy())
        return false;
    if (Ty->isTokenTy())
        return false;
    return Ty->isFirstClassType();
}

} // namespace

extern "C" {

// https://bbs.kanxue.com/thread-268789-1.htm
void amice_function_fix_stack(llvm::Function *f, int AtTerminator, int MaxIterations) {
    std::vector<llvm::PHINode *> tmpPhi;
    std::vector<llvm::Instruction *> tmpReg;
    llvm::BasicBlock *bbEntry = &*f->begin();

    int iteration = 0;
    do {
        tmpPhi.clear();
        tmpReg.clear();
        for (llvm::Function::iterator i = f->begin(); i != f->end(); i++) {
            for (llvm::BasicBlock::iterator j = i->begin(); j != i->end(); j++) {
                if (llvm::isa<llvm::PHINode>(j)) {
                    llvm::PHINode *phi = llvm::cast<llvm::PHINode>(j);
                    if (isDemotableValueTy(phi->getType())) {
                        tmpPhi.push_back(phi);
                    }
                    continue;
                }

                // 跳过 terminator（包括 invoke/switch/ret/br/callbr 等）
                if (j->isTerminator())
                    continue;

                if (!isDemotableValueTy(j->getType()))
                    continue;

                if (!(llvm::isa<llvm::AllocaInst>(j) && j->getParent() == bbEntry) &&
                    (valueEscapes(&*j) || j->isUsedOutsideOfBlock(&*i))) {
                    tmpReg.push_back(&*j);
                    continue;
                }
            }
        }
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 19)
        for (unsigned int i = 0; i < tmpReg.size(); i++) {
            if (AtTerminator) {
                llvm::DemoteRegToStack(*tmpReg.at(i), false, std::optional<llvm::BasicBlock::iterator>{bbEntry->getTerminator()});
            } else {
                llvm::DemoteRegToStack(*tmpReg.at(i));
            }
        }
        for (unsigned int i = 0; i < tmpPhi.size(); i++) {
            if (AtTerminator) {
                llvm::DemotePHIToStack(tmpPhi.at(i), std::optional<llvm::BasicBlock::iterator>{bbEntry->getTerminator()});
            } else {
                llvm::DemotePHIToStack(tmpPhi.at(i));
            }
        }
#else
        for (unsigned int i = 0; i < tmpReg.size(); i++) {
            if (AtTerminator) {
                llvm::DemoteRegToStack(*tmpReg.at(i), false, bbEntry->getTerminator());
            } else {
                llvm::DemoteRegToStack(*tmpReg.at(i));
            }
        }
        for (unsigned int i = 0; i < tmpPhi.size(); i++) {
            if (AtTerminator) {
                llvm::DemotePHIToStack(tmpPhi.at(i), bbEntry->getTerminator());
            } else {
                llvm::DemotePHIToStack(tmpPhi.at(i));
            }
        }
#endif
        iteration++;
        if (MaxIterations != 0 && iteration > MaxIterations) {
            break;
        }
    } while (tmpReg.size() != 0 || tmpPhi.size() != 0);
}

// Returns non-zero if the function is broken; on failure writes a malloc'd
// error string into *errmsg (release it with amice_free_string).
int amice_function_verify(llvm::Function &F, char **errmsg) {
    std::string err;
    llvm::raw_string_ostream rso(err);
    bool broken = llvm::verifyFunction(F, &rso);
    rso.flush();
    if (broken) {
        size_t n = err.length() + 1;
        char *p = (char *)malloc(n);
        if (p) memcpy(p, err.c_str(), n);
        *errmsg = p;
    }
    return broken;
}

bool amice_function_is_inline_marked(llvm::Function &F) {
    if (F.hasFnAttribute(llvm::Attribute::AlwaysInline)) {
        return true;
    }
    if (F.hasFnAttribute(llvm::Attribute::InlineHint)) {
        return true;
    }
    return false;
}

}

#include <functional>
#include <memory>
#include <vector>
#include <map>

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
#include "llvm/ADT/Twine.h"
#include "llvm/Analysis/AssumptionCache.h"
#include "llvm/Analysis/InlineCost.h"
#include "llvm/Transforms/Utils/ValueMapper.h"
#include "llvm/Transforms/Utils/CodeExtractor.h"

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

void amice_phi_node_remove_incoming_value(llvm::PHINode* PHI, llvm::BasicBlock* P) {
    PHI->removeIncomingValue(P);
}

void amice_phi_node_replace_incoming_block_with(llvm::PHINode* PHI, llvm::BasicBlock* O, llvm::BasicBlock* N) {
    PHI->replaceIncomingBlockWith(O, N);
}

typedef struct {
    unsigned int index;
    void* constant;
} ArgReplacement;

llvm::Function* amice_specialize_function(
    llvm::Function* originalFunc,
    llvm::Module* mod,
    const ArgReplacement* replacements,
    unsigned int replacement_count) {
#if !defined(AMICE_ENABLE_CLONE_FUNCTION)
    if (!originalFunc || !mod) {
        return nullptr;
    }

    std::map<unsigned, llvm::Constant*> replacementMap;
    for (unsigned i = 0; i < replacement_count; i++) {
        if (replacements[i].index >= originalFunc->arg_size()) {
            return nullptr; // 无效索引
        }
        replacementMap[replacements[i].index] =
            static_cast<llvm::Constant*>(replacements[i].constant);
    }

    llvm::ValueToValueMapTy VMap;
    std::vector<llvm::Type*> newArgTypes;

    unsigned argIdx = 0;
    for (const llvm::Argument& arg : originalFunc->args()) {
        if (replacementMap.count(argIdx)) {
            VMap[&arg] = replacementMap[argIdx];
        } else {
            newArgTypes.push_back(arg.getType());
        }
        argIdx++;
    }

    llvm::FunctionType* newFuncType = llvm::FunctionType::get(
        originalFunc->getFunctionType()->getReturnType(),
        newArgTypes,
        false
    );

    llvm::Function* specializedFunc = llvm::Function::Create(
        newFuncType,
        originalFunc->getLinkage(),
        originalFunc->getAddressSpace(),
        originalFunc->getName() + ".specialized.amice",
        mod
    );

    auto newArgIt = specializedFunc->arg_begin();
    argIdx = 0;
    for (const llvm::Argument& arg : originalFunc->args()) {
        if (!replacementMap.count(argIdx)) {
            VMap[&arg] = &*newArgIt;
            newArgIt->setName(arg.getName());
            ++newArgIt;
        }
        argIdx++;
    }

    llvm::SmallVector<llvm::ReturnInst*, 8> returns;
// llvm 12
//    void llvm::CloneAndPruneFunctionInto(Function *NewFunc, const Function *OldFunc,
//                                         ValueToValueMapTy &VMap,
//                                         bool ModuleLevelChanges,
//                                         SmallVectorImpl<ReturnInst*> &Returns,
//                                         const char *NameSuffix,
//                                         ClonedCodeInfo *CodeInfo,
//                                         Instruction *TheCall)
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
    llvm::CloneFunctionInto(specializedFunc, originalFunc, VMap,
        llvm::CloneFunctionChangeType::LocalChangesOnly,
        returns, "", nullptr);
#else
    llvm::CloneFunctionInto(specializedFunc, originalFunc, VMap,
        false,
        returns, "", nullptr);
#endif

    specializedFunc->copyAttributesFrom(originalFunc);

    return specializedFunc;
#else
    return nullptr;
#endif
}

llvm::CodeExtractor* amice_create_code_extractor(llvm::BasicBlock** BBs, int BBs_len) {
    std::vector<llvm::BasicBlock*> bb_vec;
    for (int i = 0; i < BBs_len; i++) {
        bb_vec.push_back(BBs[i]);
    }
    return new llvm::CodeExtractor(bb_vec);
}

void amice_delete_code_extractor(llvm::CodeExtractor* ce) {
    delete ce;
}

bool amice_code_extractor_is_eligible(llvm::CodeExtractor* ce) {
    return ce->isEligible();
}

llvm::Function* amice_code_extractor_extract_code_region(llvm::CodeExtractor* ce, llvm::Function* F) {
    llvm::CodeExtractorAnalysisCache CEAC(*F);
    return ce->extractCodeRegion(CEAC);
}

}
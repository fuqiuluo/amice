// Instruction helpers, grouped by instruction sub-kind (switch / gep / phi).
#include "amice_ffi.h"

#include <llvm/ADT/APInt.h>
#include <llvm/Config/llvm-config.h>
#include <llvm/IR/Operator.h>
#include <llvm/Transforms/Utils/Cloning.h>

extern "C" {

bool amice_instruction_has_poison_generating_flags(llvm::Instruction *I) {
#if LLVM_VERSION_MAJOR >= 14
    return I->hasPoisonGeneratingFlags();
#else
    if (auto *O = llvm::dyn_cast<llvm::OverflowingBinaryOperator>(I))
        return O->hasNoUnsignedWrap() || O->hasNoSignedWrap();
    if (auto *O = llvm::dyn_cast<llvm::PossiblyExactOperator>(I))
        return O->isExact();
    return false;
#endif
}

bool amice_call_inline(llvm::CallBase *Call) {
    llvm::InlineFunctionInfo Info;
    return llvm::InlineFunction(*Call, Info).isSuccess();
}

llvm::Function *amice_call_resolve_function(llvm::CallBase *Call) {
    return llvm::dyn_cast<llvm::Function>(
        Call->getCalledOperand()->stripPointerCastsAndAliases());
}

llvm::ConstantInt *amice_switch_find_case_dest(llvm::SwitchInst *S, llvm::BasicBlock *B) {
    return S->findCaseDest(B);
}

uint32_t amice_switch_get_case_num(llvm::SwitchInst *S) {
#if defined(LLVM_VERSION_MAJOR) && LLVM_VERSION_MAJOR >= 22
    return S->getNumCases();
#else
    return S->getNumOperands() / 2 - 1;
#endif
}

llvm::ConstantInt *amice_switch_get_case_value(llvm::SwitchInst *S, uint32_t Index) {
#if defined(LLVM_VERSION_MAJOR) && LLVM_VERSION_MAJOR >= 22
    return llvm::cast<llvm::ConstantInt>(reinterpret_cast<llvm::Value *>(
        LLVMGetSwitchCaseValue(reinterpret_cast<LLVMValueRef>(S), Index + 1)));
#else
    return llvm::cast<llvm::ConstantInt>(S->getOperand(2 + Index * 2));
#endif
}

llvm::BasicBlock *amice_switch_get_case_dest(llvm::SwitchInst *S, uint32_t Index) {
#if defined(LLVM_VERSION_MAJOR) && LLVM_VERSION_MAJOR >= 22
    return reinterpret_cast<llvm::BasicBlock *>(
        LLVMGetSuccessor(reinterpret_cast<LLVMValueRef>(S), Index + 1));
#else
    return llvm::cast<llvm::BasicBlock>(S->getOperand(3 + Index * 2));
#endif
}

bool amice_gep_accumulate_constant_offset(llvm::Instruction *I, llvm::Module *M, uint64_t *OutOffset) {
    if (auto *GEP = llvm::dyn_cast<llvm::GetElementPtrInst>(I)) {
        const llvm::DataLayout &DL = M->getDataLayout();
        llvm::APInt OffsetAI(DL.getIndexSizeInBits(/*AS=*/0), 0);
        bool result = GEP->accumulateConstantOffset(DL, OffsetAI);
        *OutOffset = OffsetAI.getZExtValue();
        return result;
    }
    return false;
}

void amice_phi_replace_incoming_block_with(llvm::PHINode *PHI, llvm::BasicBlock *O, llvm::BasicBlock *N) {
    PHI->replaceIncomingBlockWith(O, N);
}

}

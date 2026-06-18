// Instruction helpers, grouped by instruction sub-kind (switch / gep / phi).
#include "amice_ffi.h"

#include <llvm/ADT/APInt.h>
#include <llvm/Config/llvm-config.h>

extern "C" {

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

// Instruction helpers, grouped by instruction sub-kind (switch / gep / phi).
#include "amice_ffi.h"

#include <llvm/ADT/APInt.h>

extern "C" {

llvm::ConstantInt *amice_switch_find_case_dest(llvm::SwitchInst *S, llvm::BasicBlock *B) {
    return S->findCaseDest(B);
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

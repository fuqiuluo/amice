// Basic-block helpers: splitting, insertion points, and predecessor cleanup.
#include "amice_ffi.h"

extern "C" {

bool amice_basic_block_has_address_taken(llvm::BasicBlock *BB) {
    return BB->hasAddressTaken();
}

bool amice_basic_block_is_eh_pad(llvm::BasicBlock *BB) {
    return BB->isEHPad();
}

llvm::BasicBlock *amice_basic_block_split(llvm::BasicBlock *BB, llvm::Instruction *I, char *N, int B) {
#if defined(LLVM_VERSION_MAJOR) && LLVM_VERSION_MAJOR >= 23
    // LLVM 23 drops the `Before` parameter; the split-before flavour moved
    // to its own method.
    if (B) {
        return BB->splitBasicBlockBefore(I, N);
    }
    return BB->splitBasicBlock(I, N);
#else
    return BB->splitBasicBlock(I, N, B);
#endif
}

llvm::Instruction *amice_basic_block_first_insertion_pt(llvm::BasicBlock *bb) {
    return llvm::cast<llvm::Instruction>(bb->getFirstInsertionPt());
}

void amice_basic_block_remove_predecessor(llvm::BasicBlock *B, llvm::BasicBlock *P) {
    B->removePredecessor(P);
}

}

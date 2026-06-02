// Basic-block helpers: splitting, insertion points, and predecessor cleanup.
#include "amice_ffi.h"

extern "C" {

llvm::BasicBlock *amice_basic_block_split(llvm::BasicBlock *BB, llvm::Instruction *I, char *N, int B) {
    return BB->splitBasicBlock(I, N, B);
}

llvm::Instruction *amice_basic_block_first_insertion_pt(llvm::BasicBlock *bb) {
    return llvm::cast<llvm::Instruction>(bb->getFirstInsertionPt());
}

void amice_basic_block_remove_predecessor(llvm::BasicBlock *B, llvm::BasicBlock *P) {
    B->removePredecessor(P);
}

}

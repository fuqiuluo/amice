// Dominator-tree lifecycle and queries.
#include "amice_ffi.h"

#include <llvm/IR/Dominators.h>

extern "C" {

llvm::DominatorTree *amice_dominator_tree_create() {
    return new llvm::DominatorTree();
}

llvm::DominatorTree *amice_dominator_tree_create_from_function(llvm::Function *func) {
    if (!func) return nullptr;
    return new llvm::DominatorTree(*func);
}

void amice_dominator_tree_destroy(llvm::DominatorTree *dt) {
    if (dt) {
        delete dt;
    }
}

void amice_dominator_tree_view_graph(llvm::DominatorTree *dt) {
    dt->viewGraph();
}

// Whether basic block A dominates basic block B.
bool amice_dominator_tree_dominates(llvm::DominatorTree *dt, llvm::BasicBlock *A, llvm::BasicBlock *B) {
    if (!dt || !A || !B) return false;
    return dt->dominates(A, B);
}

}

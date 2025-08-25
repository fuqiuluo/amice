#include "llvm/IR/Dominators.h"
#include "llvm/IR/Function.h"
#include "llvm/IR/BasicBlock.h"
#include "llvm/IR/Instruction.h"
#include "llvm/IR/Value.h"
#include "llvm/IR/Use.h"
#include "llvm/ADT/Twine.h"
#include <memory>

extern "C" {

// DominatorTree lifecycle management
llvm::DominatorTree* llvm_dominator_tree_create() {
    auto* dt = new llvm::DominatorTree();
    return dt;
}

llvm::DominatorTree* llvm_dominator_tree_create_from_function(llvm::Function* func) {
    if (!func) return nullptr;
    auto* dt = new llvm::DominatorTree(*func);
    return dt;
}

void llvm_dominator_tree_destroy(llvm::DominatorTree* dt) {
    if (dt) {
        delete dt;
    }
}

void llvm_dominator_tree_view_graph(llvm::DominatorTree* dt) {
    dt->viewGraph();
}

bool llvm_dominator_tree_dominate_BU(llvm::DominatorTree* dt, llvm::BasicBlock* B, llvm::Use& U) {
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 12)
    //llvm::BasicBlock* BB = (llvm::BasicBlock*)&*U;
    return dt->dominates(B, U);
#else
    const llvm::DomTreeNodeBase<llvm::BasicBlock> *NA = dt->getNode(B);
    const llvm::DomTreeNodeBase<llvm::BasicBlock> *NB = dt->getNode((llvm::BasicBlock*)&*U);
    if (!NA || !NB) return false;

    return dt->dominates(NA, NB);
#endif
}

} // extern "C"
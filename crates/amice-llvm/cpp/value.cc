// Value-level helpers for use-list rewriting.
#include "amice_ffi.h"

#include <llvm/IR/Value.h>

extern "C" {

void amice_value_replace_non_metadata_uses_with(llvm::Value *V, llvm::Value *NewV) {
    V->replaceNonMetadataUsesWith(NewV);
}

void amice_value_drop_droppable_uses(llvm::Value *V) {
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 12)
    V->dropDroppableUses();
#endif
}

bool amice_value_has_undroppable_uses(llvm::Value *V) {
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 12)
    return V->hasNUndroppableUsesOrMore(1);
#else
    return !V->use_empty();
#endif
}

}

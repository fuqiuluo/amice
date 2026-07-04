// Value-level helpers for use-list rewriting.
#include "amice_ffi.h"

#include <llvm/IR/Metadata.h>
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

const char *amice_value_metadata_string(llvm::Value *V) {
    auto *MAV = llvm::dyn_cast_or_null<llvm::MetadataAsValue>(V);
    if (MAV == nullptr) {
        return nullptr;
    }

    auto *MDS = llvm::dyn_cast_or_null<llvm::MDString>(MAV->getMetadata());
    if (MDS == nullptr) {
        return nullptr;
    }

    std::string Text = MDS->getString().str();
    char *Out = static_cast<char *>(std::malloc(Text.size() + 1));
    if (Out == nullptr) {
        return nullptr;
    }
    std::memcpy(Out, Text.data(), Text.size());
    Out[Text.size()] = '\0';
    return Out;
}

}

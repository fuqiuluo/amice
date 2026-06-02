// CodeExtractor lifecycle and region extraction.
#include "amice_ffi.h"

#include <llvm/Transforms/Utils/CodeExtractor.h>

extern "C" {

llvm::CodeExtractor *amice_code_extractor_create(llvm::BasicBlock **BBs, int BBs_len) {
    std::vector<llvm::BasicBlock *> bb_vec;
    for (int i = 0; i < BBs_len; i++) {
        bb_vec.push_back(BBs[i]);
    }
    return new llvm::CodeExtractor(bb_vec);
}

void amice_code_extractor_delete(llvm::CodeExtractor *ce) {
    delete ce;
}

bool amice_code_extractor_is_eligible(llvm::CodeExtractor *ce) {
    return ce->isEligible();
}

llvm::Function *amice_code_extractor_extract_region(llvm::CodeExtractor *ce, llvm::Function *F) {
    llvm::CodeExtractorAnalysisCache CEAC(*F);
    return ce->extractCodeRegion(CEAC);
}

}

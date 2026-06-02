// Module-level helpers: global ctor/used lists, function specialization,
// and constant-array construction.
#include "amice_ffi.h"

#include <map>

#include <llvm/Transforms/Utils/Cloning.h>
#include <llvm/Transforms/Utils/ModuleUtils.h>
#include <llvm/Transforms/Utils/ValueMapper.h>

extern "C" {

void amice_module_append_to_global_ctors(llvm::Module &M, llvm::Function *F, int P) {
    llvm::appendToGlobalCtors(M, F, P);
}

void amice_module_append_to_used(llvm::Module &M, llvm::GlobalValue *V) {
    llvm::appendToUsed(M, {V});
}

void amice_module_append_to_compiler_used(llvm::Module &M, llvm::GlobalValue *V) {
    llvm::appendToCompilerUsed(M, {V});
}

typedef struct {
    unsigned int index;
    void *constant;
} ArgReplacement;

llvm::Function *amice_module_specialize_function(
    llvm::Function *originalFunc,
    llvm::Module *mod,
    const ArgReplacement *replacements,
    unsigned int replacement_count) {
#if !defined(AMICE_ENABLE_CLONE_FUNCTION)
    if (!originalFunc || !mod) {
        return nullptr;
    }

    std::map<unsigned, llvm::Constant *> replacementMap;
    for (unsigned i = 0; i < replacement_count; i++) {
        if (replacements[i].index >= originalFunc->arg_size()) {
            return nullptr; // 无效索引
        }
        replacementMap[replacements[i].index] =
            static_cast<llvm::Constant *>(replacements[i].constant);
    }

    llvm::ValueToValueMapTy VMap;
    std::vector<llvm::Type *> newArgTypes;

    unsigned argIdx = 0;
    for (const llvm::Argument &arg : originalFunc->args()) {
        if (replacementMap.count(argIdx)) {
            VMap[&arg] = replacementMap[argIdx];
        } else {
            newArgTypes.push_back(arg.getType());
        }
        argIdx++;
    }

    llvm::FunctionType *newFuncType = llvm::FunctionType::get(
        originalFunc->getFunctionType()->getReturnType(),
        newArgTypes,
        false);

    llvm::Function *specializedFunc = llvm::Function::Create(
        newFuncType,
        originalFunc->getLinkage(),
        originalFunc->getAddressSpace(),
        originalFunc->getName() + ".specialized.amice",
        mod);

    auto newArgIt = specializedFunc->arg_begin();
    argIdx = 0;
    for (const llvm::Argument &arg : originalFunc->args()) {
        if (!replacementMap.count(argIdx)) {
            VMap[&arg] = &*newArgIt;
            newArgIt->setName(arg.getName());
            ++newArgIt;
        }
        argIdx++;
    }

    llvm::SmallVector<llvm::ReturnInst *, 8> returns;
#if defined(LLVM_VERSION_MAJOR) && (LLVM_VERSION_MAJOR >= 13)
    llvm::CloneFunctionInto(specializedFunc, originalFunc, VMap,
        llvm::CloneFunctionChangeType::LocalChangesOnly,
        returns, "", nullptr);
#else
    llvm::CloneFunctionInto(specializedFunc, originalFunc, VMap,
        false,
        returns, "", nullptr);
#endif

    specializedFunc->copyAttributesFrom(originalFunc);

    return specializedFunc;
#else
    return nullptr;
#endif
}

LLVMValueRef amice_module_const_array(LLVMTypeRef element_ty, LLVMValueRef *values, uint64_t len) {
    llvm::Type *ElemTy = llvm::unwrap(element_ty);
    llvm::ArrayRef<llvm::Constant *> Vals(llvm::unwrap<llvm::Constant>(values, len), len);
    llvm::ArrayType *ArrayTy = llvm::ArrayType::get(ElemTy, len);
    return llvm::wrap(llvm::ConstantArray::get(ArrayTy, Vals));
}

}

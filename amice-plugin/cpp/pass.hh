#pragma once

#include <memory>
#include <type_traits>
#include <utility>

#include <llvm/IR/PassManager.h>

namespace {
enum class PreservedAnalyses {
  kAll,
  kNone,
};

template <typename IR> struct Pass : public llvm::PassInfoMixin<Pass<IR>> {
  using DataPtr = const void *;
  using DataDeleter = void (*)(DataPtr);
  using Data = std::unique_ptr<std::remove_pointer_t<DataPtr>, DataDeleter>;

  using Entrypoint = PreservedAnalyses (*)(DataPtr, typename IR::Unit &,
                                           typename IR::AnalysisManager &);

  Pass(Entrypoint Func, Data PassData) : PassData(std::move(PassData)) {
    this->Func = Func;
  }

  auto run(typename IR::Unit &IrUnit, typename IR::AnalysisManager &AM)
      -> llvm::PreservedAnalyses {
    return (this->Func(this->PassData.get(), IrUnit, AM) ==
                    PreservedAnalyses::kAll
                ? llvm::PreservedAnalyses::all()
                : llvm::PreservedAnalyses::none());
  }

private:
  Entrypoint Func;
  Data PassData;
};

} // namespace

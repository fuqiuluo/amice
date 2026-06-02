#pragma once

#include <memory>
#include <mutex>
#include <type_traits>
#include <utility>

#include <llvm/IR/PassManager.h>

namespace {

template <typename IR>
struct Analysis : public llvm::AnalysisInfoMixin<Analysis<IR>> {
  using DataPtr = const void *;
  using DataDeleter = void (*)(DataPtr);
  using Data = std::unique_ptr<std::remove_pointer_t<DataPtr>, DataDeleter>;

  using ResultPtr = void *;
  using ResultDeleter = void (*)(ResultPtr);
  using Result =
      std::unique_ptr<std::remove_pointer_t<ResultPtr>, ResultDeleter>;

  using Entrypoint = void (*)(DataPtr, typename IR::Unit &,
                              typename IR::AnalysisManager &, ResultPtr &,
                              ResultDeleter &);

  Analysis(Entrypoint Func, Data AnalysisData)
      : AnalysisData(std::move(AnalysisData)) {
    this->Func = Func;
  }

  auto run(typename IR::Unit &IrUnit, typename IR::AnalysisManager &AM)
      -> Result {
    auto *Ptr = (ResultPtr) nullptr;
    auto Deleter = (ResultDeleter) nullptr;
    this->Func(this->AnalysisData.get(), IrUnit, AM, Ptr, Deleter);

    return {Ptr, Deleter};
  }

  static auto ID() // NOLINT(readability-identifier-naming)
      -> llvm::AnalysisKey * {
    return CurrentKey;
  }

  static inline auto CurrentKey = (llvm::AnalysisKey *)nullptr;
  static inline auto MutexCurrentKey = std::mutex{};

private:
  Entrypoint Func;
  Data AnalysisData;

  friend struct llvm::AnalysisInfoMixin<Analysis<IR>>;
};

} // namespace

#pragma once

#include <llvm/IR/Function.h>
#include <llvm/IR/Module.h>
#include <llvm/IR/PassManager.h>

struct ModuleIR {
  using AnalysisManager = llvm::ModuleAnalysisManager;
  using Unit = llvm::Module;
};

struct FunctionIR {
  using AnalysisManager = llvm::FunctionAnalysisManager;
  using Unit = llvm::Function;
};

use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::{Config, FlattenMode, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::basic_block::split_basic_block;
use amice_llvm::ir::function::fix_stack;
use amice_llvm::module_utils::{VerifyResult, verify_function, verify_function2};
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, IntValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{Level, debug, error, log_enabled, warn};
use rand::Rng;
use std::collections::HashMap;

#[amice(priority = 1111, name = "CloneFunction", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct CloneFunction {
    enable: bool,
}

impl AmicePassLoadable for CloneFunction {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.clone_function.enable;
       
        self.enable
    }
}

impl LlvmModulePass for CloneFunction {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }


        PreservedAnalyses::None
    }
}

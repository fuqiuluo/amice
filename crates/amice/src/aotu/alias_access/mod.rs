mod pointer_chain;

use crate::aotu::alias_access::pointer_chain::PointerChainAlgo;
use crate::config::{AliasAccessConfig, AliasAccessMode, Config};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag, AmicePassMetadata};
use amice_llvm::inkwell2::{FunctionExt, VerifyResult};
use amice_macro::amice;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use amice_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};

#[amice(
    priority = 1112,
    name = "AliasAccess",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = AliasAccessConfig,
)]
#[derive(Default)]
pub struct AliasAccess {}

impl AmicePass for AliasAccess {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.alias_access.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut functions = Vec::new();
        for function in module.get_functions() {
            if function.is_llvm_function() {
                continue;
            }

            if function.is_inline_marked() {
                continue;
            }

            if function.is_undef_function() {
                continue;
            }

            // 含异常处理构造的函数（invoke/catchpad/cleanuppad 等）不做别名改写：
            // catchpad 的 catch 对象存储槽必须是 alloca，运行时按 .xdata 里的
            // 固定帧偏移写入；指针链替换后 funclet 读到从未被写入的槽，首个
            // 异常分发即 SIGSEGV（issue #102）
            if function_has_exception_handling(function) {
                debug!(
                    "function {:?} has exception handling instructions, skipping",
                    function.get_name()
                );
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            functions.push((function, cfg));
        }

        if functions.is_empty() {
            return Ok(PreservedAnalyses::All);
        }

        for (function, cfg) in functions {
            let mut algo: Box<dyn AliasAccessAlgo> = match cfg.mode {
                AliasAccessMode::PointerChain => Box::new(PointerChainAlgo::default()),
            };

            if let Err(e) = algo.initialize(&cfg) {
                error!("failed to initialize: {}", e);
                return Ok(PreservedAnalyses::All);
            }

            if let Err(e) = algo.do_alias_access(&cfg, module, function) {
                error!("failed to do alias access: {}", e);
                return Ok(PreservedAnalyses::All);
            }

            if let VerifyResult::Broken(e) = function.verify_function() {
                warn!("function {:?} verify failed: {}", function.get_name(), e);
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

/// 含异常处理构造的函数不参与别名改写（issue #102）
fn function_has_exception_handling(function: FunctionValue<'_>) -> bool {
    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            if matches!(
                inst.get_opcode(),
                InstructionOpcode::Invoke
                    | InstructionOpcode::LandingPad
                    | InstructionOpcode::Resume
                    | InstructionOpcode::CatchSwitch
                    | InstructionOpcode::CatchPad
                    | InstructionOpcode::CatchRet
                    | InstructionOpcode::CleanupPad
                    | InstructionOpcode::CleanupRet
                    | InstructionOpcode::CallBr
            ) {
                return true;
            }
        }
    }

    false
}

pub(crate) trait AliasAccessAlgo {
    fn initialize(&mut self, pass: &AliasAccessConfig) -> anyhow::Result<()>;

    fn do_alias_access<'ctx>(
        &mut self,
        pass: &AliasAccessConfig,
        module: &Module<'ctx>,
        function: FunctionValue<'ctx>,
    ) -> anyhow::Result<()>;
}

//! BCF combines integer region rewriting with guarded whole-function copies.
mod clone;
mod region;

use crate::config::{BogusControlFlowConfig, Config};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{FunctionExt, InstructionExt};
use amice_macro::amice;
use amice_plugin::PreservedAnalyses;
use amice_plugin::inkwell::comdat::ComdatSelectionKind;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use anyhow::Context;

fn has_entry_block_intrinsics(function: FunctionValue<'_>) -> bool {
    // Both transformations split the entry block. These intrinsics must stay
    // there; moving their operands is not generally safe (escaped stack slots,
    // GC registration or a convergence token).
    function.get_first_basic_block().is_some_and(|entry| {
        entry.get_instructions().any(|instruction| {
            instruction.get_opcode() == InstructionOpcode::Call
                && instruction.into_call_inst().get_call_function().is_some_and(|callee| {
                    matches!(
                        callee.get_name().to_bytes(),
                        b"llvm.localescape" | b"llvm.gcroot" | b"llvm.experimental.convergence.entry"
                    )
                })
        })
    })
}

#[amice(
    priority = 970,
    name = "BogusControlFlow",
    flag = AmicePassFlag::OptimizerLast | AmicePassFlag::FullLtoLast | AmicePassFlag::FunctionLevel,
    config = BogusControlFlowConfig,
)]
#[derive(Default)]
pub struct BogusControlFlow {}

impl AmicePass for BogusControlFlow {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.bogus_control_flow.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut changed = false;
        let functions = module.get_functions().collect::<Vec<_>>();
        for function in functions {
            if function.is_llvm_function() || function.is_undef_function() {
                continue;
            }
            // Independent translation units normally have different random
            // seeds. These COMDAT kinds require matching bytes or size across
            // definitions, so neither BCF transformation may change their body.
            if function.as_global_value().get_comdat().is_some_and(|comdat| {
                matches!(
                    comdat.get_selection_kind(),
                    ComdatSelectionKind::ExactMatch | ComdatSelectionKind::SameSize
                )
            }) {
                continue;
            }
            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }
            let seed = function
                .get_name()
                .to_bytes()
                .iter()
                .fold(cfg.seed, |s, b| (s ^ u64::from(*b)).wrapping_mul(0x100000001b3));
            changed |= (|| -> anyhow::Result<bool> {
                // 成功路径内部已处理真实函数的整数区域，提交后无需再执行一次。
                if clone::insert_bogus_branch(module, function, &cfg, seed)? {
                    return Ok(true);
                }
                // 克隆补充未提交时，原函数体保持原状，回退到原有区域变换。
                Ok(region::lower_to_digit_loops(module, function, &cfg, seed)? != 0)
            })()
            .with_context(|| format!("rewriting BCF in {}", function.get_name().to_string_lossy()))?;
        }
        Ok(if changed {
            PreservedAnalyses::None
        } else {
            PreservedAnalyses::All
        })
    }
}

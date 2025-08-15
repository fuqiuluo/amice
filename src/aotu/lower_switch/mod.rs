use crate::config::Config;
use crate::llvm_utils::function::get_basic_block_entry;
use crate::llvm_utils::switch_inst;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::module_utils::{verify_function, verify_function2};
use amice_macro::amice;
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::llvm_sys::core::LLVMAddIncoming;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{
    AsValueRef, BasicValue, FunctionValue, InstructionOpcode, InstructionValue, PhiValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{error, warn};

#[amice(priority = 961, name = "LowerSwitch", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct LowerSwitch {
    enable: bool,
    append_dummy_code: bool,
}

impl AmicePassLoadable for LowerSwitch {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.lower_switch.enable;
        self.append_dummy_code = cfg.lower_switch.append_dummy_code;

        self.enable
    }
}

impl LlvmModulePass for LowerSwitch {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(e) = do_lower_switch(module, function, self.append_dummy_code) {
                error!("Failed to lower switch in function {:?}: {}", function.get_name(), e);
            }
        }

        for f in module.get_functions() {
            if verify_function2(f.as_value_ref() as *mut std::ffi::c_void) {
                warn!("(lower-switch) function {:?} is not verified", f.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

fn do_lower_switch(module: &mut Module<'_>, function: FunctionValue, append_dummy_code: bool) -> anyhow::Result<()> {
    let switch_inst_list = function
        .get_basic_blocks()
        .into_iter()
        .filter_map(|bb| bb.get_terminator())
        .filter(|inst| inst.get_opcode() == InstructionOpcode::Switch)
        .collect::<Vec<_>>();

    if switch_inst_list.is_empty() {
        return Ok(());
    }

    for inst in switch_inst_list {
        demote_switch_to_if(module, function, inst, append_dummy_code)?;
    }

    Ok(())
}

pub(crate) fn demote_switch_to_if(
    module: &mut Module<'_>,
    function: FunctionValue,
    inst: InstructionValue,
    append_dummy_code: bool,
) -> anyhow::Result<()> {
    let switch_block = inst
        .get_parent()
        .ok_or_else(|| anyhow::anyhow!("Switch instruction has no parent block"))?;
    let default = switch_inst::get_default_block(inst);
    let condition = switch_inst::get_condition(inst);
    let cases = switch_inst::get_cases(inst);

    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i32_zero = i32_ty.const_zero();
    let i32_one = i32_ty.const_int(1, false);
    let condition_ty = condition.get_type();

    if !condition_ty.is_int_type() {
        return Err(anyhow::anyhow!("Unsupported condition type: {:?}", condition_ty));
    }

    let builder = ctx.create_builder();
    if cases.is_empty() {
        builder.position_before(&inst);
        builder.build_unconditional_branch(default)?;
        inst.erase_from_basic_block();
        return Ok(());
    }

    let unreachable_block = ctx.append_basic_block(function, "unreachable");
    builder.position_at_end(unreachable_block);
    builder.build_unreachable()?;

    let mut lower_branches = Vec::new();
    let mut current_branch = ctx.append_basic_block(function, "lower_switch_branch");
    for (case, dest) in cases {
        let next_branch = ctx.append_basic_block(function, "lower_switch_branch");
        builder.position_at_end(current_branch);
        let cond =
            builder.build_int_compare(IntPredicate::EQ, condition.into_int_value(), case.into_int_value(), "")?;
        update_phi_nodes(switch_block, current_branch, dest);
        builder.build_conditional_branch(cond, dest, next_branch)?;

        lower_branches.push(current_branch);

        current_branch = next_branch;
    }

    let mut dummy_value_ptr = None;
    if append_dummy_code && let Some(entry_block) = get_basic_block_entry(function) {
        builder.position_before(&entry_block.get_terminator().unwrap());
        let tmp = builder.build_alloca(i32_ty, ".tmp")?;
        builder.build_store(tmp, i32_zero)?;

        builder.position_before(&inst);
        let dummy_value = builder.build_load(i32_ty, tmp, "")?;
        let cond = builder.build_int_compare(IntPredicate::EQ, dummy_value.into_int_value(), i32_zero, "")?;
        builder.build_conditional_branch(cond, lower_branches[0], unreachable_block)?;
        dummy_value_ptr = Some(tmp);
    } else {
        builder.position_before(&inst);
        builder.build_unconditional_branch(lower_branches[0])?;
    }

    if append_dummy_code
        && let Some(_entry_block) = get_basic_block_entry(function)
        && let Some(case_last) = lower_branches.last()
        && let Some(dummy_value_ptr) = dummy_value_ptr
    {
        builder.position_at_end(current_branch);
        let phi = builder.build_phi(i32_ty, "lower_switch_phi")?;
        phi.add_incoming(&[(&i32_zero, *case_last), (&i32_one, switch_block)]);
        builder.build_store(dummy_value_ptr, phi.as_basic_value())?;
        let dummy_value = builder.build_load(i32_ty, dummy_value_ptr, "")?;
        let cond =
            builder.build_int_compare(IntPredicate::EQ, dummy_value.into_int_value(), i32_zero, "switch_cond")?;
        builder.build_conditional_branch(cond, default, unreachable_block)?;
    } else {
        builder.position_at_end(current_branch);
        builder.build_unconditional_branch(default)?;
    }
    update_phi_nodes(switch_block, current_branch, default);

    inst.erase_from_basic_block();

    Ok(())
}

fn update_phi_nodes<'ctx>(old_pred: BasicBlock<'ctx>, new_pred: BasicBlock<'ctx>, target_block: BasicBlock<'ctx>) {
    for phi in target_block.get_first_instruction().iter() {
        if phi.get_opcode() != InstructionOpcode::Phi {
            break;
        }

        let phi = unsafe { PhiValue::new(phi.as_value_ref()) };
        let incoming_vec = phi
            .get_incomings()
            .filter_map(|(value, pred)| {
                if pred == old_pred {
                    (value, new_pred).into()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let (mut values, mut basic_blocks): (Vec<LLVMValueRef>, Vec<LLVMBasicBlockRef>) = {
            incoming_vec
                .iter()
                .map(|&(v, bb)| (v.as_value_ref(), bb.as_mut_ptr()))
                .unzip()
        };

        unsafe {
            LLVMAddIncoming(
                phi.as_value_ref(),
                values.as_mut_ptr(),
                basic_blocks.as_mut_ptr(),
                incoming_vec.len() as u32,
            );
        }
    }
}

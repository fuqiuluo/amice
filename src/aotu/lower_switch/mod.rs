use crate::config::{Config, LowerSwitchConfig};
use crate::pass_registry::{AmiceFunctionPass, AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt, SwitchInst};
use amice_macro::amice;
use llvm_plugin::inkwell::IntPredicate;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode, InstructionValue};
use llvm_plugin::{PreservedAnalyses};

#[amice(
    priority = 961,
    name = "LowerSwitch",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::FunctionLevel,
    config = LowerSwitchConfig,
)]
#[derive(Default)]
pub struct LowerSwitch {
}

impl AmicePass for LowerSwitch {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.lower_switch.clone();
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        let mut executed = false;
        for function in module.get_functions() {
            if function.is_undef_function() || function.is_llvm_function() {
                continue;
            }

            let cfg = self.parse_function_annotations(module, function)?;
            if !cfg.enable {
                continue;
            }

            if let Err(e) = do_lower_switch(module, function, cfg.append_dummy_code) {
                error!("Failed to lower switch in function {:?}: {}", function.get_name(), e);
            }

            executed = true;
            if function.verify_function_bool() {
                warn!("function {:?} is not verified", function.get_name());
            }
        }
        
        if !executed {
            return Ok(PreservedAnalyses::All);
        }

        Ok(PreservedAnalyses::None)
    }
}

fn do_lower_switch(module: &mut Module<'_>, function: FunctionValue, append_dummy_code: bool) -> anyhow::Result<()> {
    let switch_inst_list = function
        .get_basic_blocks()
        .into_iter()
        .filter_map(|bb| bb.get_terminator())
        .filter(|inst| inst.get_opcode() == InstructionOpcode::Switch)
        .map(|inst| inst.into_switch_inst())
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
    inst: SwitchInst,
    append_dummy_code: bool,
) -> anyhow::Result<()> {
    let switch_block = inst
        .get_parent()
        .ok_or_else(|| anyhow::anyhow!("Switch instruction has no parent block"))?;
    let default = inst.get_default_block();
    let condition = inst.get_condition();
    let cases = inst.get_cases();

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
        dest.fix_phi_node(switch_block, current_branch);
        builder.build_conditional_branch(cond, dest, next_branch)?;

        lower_branches.push(current_branch);

        current_branch = next_branch;
    }

    let mut dummy_value_ptr = None;
    if append_dummy_code && let Some(entry_block) = function.get_entry_block() {
        builder.position_before(&entry_block.get_terminator().unwrap());
        let tmp = builder.build_alloca(i32_ty, ".tmp")?;
        builder.build_store(tmp, i32_zero)?;

        builder.position_before(&inst);
        let dummy_value = builder.build_load2(i32_ty, tmp, "")?;
        let cond = builder.build_int_compare(IntPredicate::EQ, dummy_value.into_int_value(), i32_zero, "")?;
        builder.build_conditional_branch(cond, lower_branches[0], unreachable_block)?;
        dummy_value_ptr = Some(tmp);
    } else {
        builder.position_before(&inst);
        builder.build_unconditional_branch(lower_branches[0])?;
    }

    if append_dummy_code
        && let Some(_entry_block) = function.get_entry_block()
        && let Some(case_last) = lower_branches.last()
        && let Some(dummy_value_ptr) = dummy_value_ptr
    {
        builder.position_at_end(current_branch);
        let phi = builder.build_phi(i32_ty, "lower_switch_phi")?;
        phi.add_incoming(&[(&i32_zero, *case_last), (&i32_one, switch_block)]);
        builder.build_store(dummy_value_ptr, phi.as_basic_value())?;
        let dummy_value = builder.build_load2(i32_ty, dummy_value_ptr, "")?;
        let cond =
            builder.build_int_compare(IntPredicate::EQ, dummy_value.into_int_value(), i32_zero, "switch_cond")?;
        builder.build_conditional_branch(cond, default, unreachable_block)?;
    } else {
        builder.position_at_end(current_branch);
        builder.build_unconditional_branch(default)?;
    }
    default.fix_phi_node(switch_block, current_branch);

    inst.erase_from_basic_block();

    Ok(())
}

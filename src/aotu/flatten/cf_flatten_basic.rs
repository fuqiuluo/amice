use crate::aotu::flatten::{Flatten, FlattenAlgo, split_entry_block_for_flatten};
use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::FlattenConfig;
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt, LLVMBasicBlockRefExt, PhiInst};
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{FunctionValue, InstructionOpcode};
use log::warn;
use rand::Rng;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct FlattenBasic;

impl FlattenAlgo for FlattenBasic {
    fn initialize(&mut self, _cfg: &FlattenConfig, _module: &mut Module<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn do_flatten(
        &mut self,
        cfg: &FlattenConfig,
        module: &mut Module<'_>,
        function: FunctionValue,
    ) -> anyhow::Result<()> {
        if function.count_basic_blocks() <= 2 {
            return Ok(());
        }

        if cfg.skip_big_function && function.count_basic_blocks() > 4096 {
            warn!(
                "(Flatten) function {:?} has too many basic blocks, skipping",
                function.get_name()
            );
            return Ok(());
        }

        for _ in 0..cfg.loop_count {
            if let Err(err) = do_handle(module, function, cfg.lower_switch) {
                warn!("(Flatten) function {:?} failed: {}", function.get_name(), err);
                return Ok(());
            }

            if cfg.skip_big_function && function.count_basic_blocks() > 4096 {
                break;
            }
        }

        if cfg.fix_stack {
            unsafe {
                function.fix_stack();
            }
        }

        Ok(())
    }
}

fn do_handle(module: &mut Module<'_>, function: FunctionValue, demote_switch: bool) -> anyhow::Result<()> {
    let Some(entry_block) = function.get_entry_block() else {
        return Err(anyhow::anyhow!(
            "(flatten) function {:?} has no entry block",
            function.get_name()
        ));
    };

    // Check all basic blocks for exception handling instructions
    let mut has_exception_handling = false;
    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            if matches!(
                inst.get_opcode(),
                InstructionOpcode::Invoke
                    | InstructionOpcode::LandingPad
                    | InstructionOpcode::CatchSwitch
                    | InstructionOpcode::CatchPad
                    | InstructionOpcode::CatchRet
                    | InstructionOpcode::CleanupPad
                    | InstructionOpcode::CallBr
            ) {
                has_exception_handling = true;
                break;
            }
        }
        if has_exception_handling {
            break;
        }
    }
    if has_exception_handling {
        // 跳过该函数，不做扁平化
        warn!(
            "(flatten) function {:?} has exception handling instructions, skipping",
            function.get_name()
        );
        return Ok(());
    }

    let mut basic_blocks = function.get_basic_blocks();
    basic_blocks.retain(|bb| bb != &entry_block);
    if !split_entry_block_for_flatten(function, entry_block, &mut basic_blocks)? {
        // 切割失败，未知的终结指令！？或者是可忽略的
        // 这并不是错误，是可预期的！
        return Ok(());
    }

    let entry_terminator = entry_block.get_terminator().ok_or(anyhow::anyhow!(
        "(flatten) function {:?} has no entry terminator",
        function.get_name()
    ))?;
    let basic_block_mapping = generate_basic_block_mapping(&basic_blocks);

    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let builder = ctx.create_builder();

    let dispatch_cases = basic_block_mapping
        .iter()
        .map(|(k, v)| (i32_ty.const_int(*v as u64, false), k))
        .map(|(v, bb)| (v, bb.into_basic_block()))
        .filter_map(|(v, bb)| {
            if let Some(bb) = bb {
                Some((v, bb))
            } else {
                warn!("(flatten) basic block {:?} not found in mapping", bb);
                None
            }
        })
        .collect::<Vec<_>>();

    let first_block = basic_blocks[0];
    let first_block_dispatch_id = basic_block_mapping[&first_block.as_mut_ptr()];

    let dispatcher = ctx.append_basic_block(function, "dispatcher");
    let default = ctx.append_basic_block(function, "default");
    dispatcher.move_before(first_block).expect("failed to move basic block");
    default.move_after(dispatcher).expect("failed to move basic block");

    let first_insertion_pt = entry_block.get_first_insertion_pt();
    builder.position_before(&first_insertion_pt);
    let dispatch_id_ptr = builder.build_alloca(i32_ty, "")?;

    builder.position_before(&entry_terminator);
    builder.build_store(dispatch_id_ptr, i32_ty.const_int(first_block_dispatch_id as u64, false))?;
    builder.build_unconditional_branch(dispatcher)?;

    entry_terminator.erase_from_basic_block();

    builder.position_at_end(dispatcher);
    let dispatch_id = builder.build_load2(i32_ty, dispatch_id_ptr, "")?;
    builder.build_switch(dispatch_id.into_int_value(), default, &dispatch_cases)?;

    builder.position_at_end(default);
    builder.build_unconditional_branch(dispatcher)?;

    let mut unconditional_br = Vec::new();
    let mut conditional_br = Vec::new();
    let mut switch = Vec::new();

    for bb in basic_blocks {
        bb.move_before(dispatcher)
            .expect("failed to move basic block after dispatcher");
        if let Some(terminator) = bb.get_terminator() {
            match terminator.get_opcode() {
                InstructionOpcode::Br => {
                    if terminator.is_conditional() {
                        conditional_br.push(terminator.into_branch_inst());
                    } else {
                        unconditional_br.push(terminator.into_branch_inst());
                    }
                },
                InstructionOpcode::Switch => {
                    switch.push(terminator.into_switch_inst());
                },
                _ => continue, // 其他类型的终结指令不处理
            }
        } else {
            warn!("(flatten) basic block {:?} has no terminator", bb);
        }
    }

    for terminator in unconditional_br {
        let successor_block = terminator.get_successor(0).unwrap();
        let dispatch_id = basic_block_mapping[&successor_block.as_mut_ptr()];
        let dispatch_id = i32_ty.const_int(dispatch_id as u64, false);
        builder.position_before(&terminator);
        builder.build_store(dispatch_id_ptr, dispatch_id)?;
        builder.build_unconditional_branch(dispatcher)?;
        terminator.erase_from_basic_block();
    }

    for terminator in conditional_br {
        let successor_true = terminator.get_successor(0).unwrap();
        let successor_false = terminator.get_successor(1).unwrap();
        let dispatch_id_true = basic_block_mapping[&successor_true.as_mut_ptr()];
        let dispatch_id_false = basic_block_mapping[&successor_false.as_mut_ptr()];
        let dispatch_id_true = i32_ty.const_int(dispatch_id_true as u64, false);
        let dispatch_id_false = i32_ty.const_int(dispatch_id_false as u64, false);
        let cond = terminator.get_operand(0).unwrap().value().unwrap().into_int_value();
        builder.position_before(&terminator);
        let successor_id = builder
            .build_select(cond, dispatch_id_true, dispatch_id_false, "")?
            .into_int_value();
        builder.build_store(dispatch_id_ptr, successor_id)?;
        builder.build_unconditional_branch(dispatcher)?;
        terminator.erase_from_basic_block();
    }

    if demote_switch {
        for terminator in switch {
            if let Err(e) = demote_switch_to_if(module, function, terminator, false) {
                warn!("(flatten) failed to demote switch to if: {}", e);
                continue;
            }
        }
    }

    Ok(())
}

fn generate_basic_block_mapping(basic_blocks: &[BasicBlock]) -> HashMap<LLVMBasicBlockRef, u32> {
    let mut rng = rand::rng();
    let mut mapping = HashMap::new();
    let mut values = Vec::with_capacity(basic_blocks.len());
    for bb in basic_blocks {
        let mut unique = rng.random::<u32>();
        while values.contains(&unique) {
            unique = rng.random();
        }
        values.push(unique);
        mapping.insert(bb.as_mut_ptr() as LLVMBasicBlockRef, unique);
    }

    mapping
}

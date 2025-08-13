use crate::aotu::lower_switch::demote_switch_to_if;
use crate::config::{Config, IndirectBranchFlags};
use crate::llvm_utils::basic_block::split_basic_block;
use crate::llvm_utils::branch_inst;
use crate::llvm_utils::function::get_basic_block_entry;
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::function::fix_stack;
use amice_llvm::module_utils::verify_function;
use amice_macro::amice;
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMBasicBlockRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, IntValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};
use rand::Rng;
use std::collections::HashMap;

#[amice(priority = 959, name = "Flatten", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct Flatten {
    enable: bool,
    fix_stack: bool,
    demote_switch: bool,
}

impl AmicePassLoadable for Flatten {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.flatten.enable;
        self.fix_stack = cfg.flatten.fix_stack;
        self.demote_switch = cfg.flatten.lower_switch;

        if !self.fix_stack && !self.demote_switch {
            // switch降级没有开启且fixStack也没有开启意味着PHI 99%有问题！
            error!(
                "(flatten) both fix_stack and lower_switch are disabled, this will likely cause issues with PHI nodes"
            );
            // 给个警告，然后听天由命，这个是用户自己决定的，hhh
        }

        self.enable
    }
}

impl LlvmModulePass for Flatten {
    fn run_pass(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if function.count_basic_blocks() <= 2 {
                continue;
            }

            let mut has_eh_or_invoke = false;
            'outer: for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    match inst.get_opcode() {
                        InstructionOpcode::Invoke // TODO: support it!
                        | InstructionOpcode::LandingPad
                        | InstructionOpcode::CatchSwitch
                        | InstructionOpcode::CatchPad
                        | InstructionOpcode::CatchRet
                        | InstructionOpcode::CleanupPad
                        | InstructionOpcode::CallBr => {
                            has_eh_or_invoke = true;
                            break 'outer;
                        },
                        _ => {},
                    }
                }
            }
            if has_eh_or_invoke {
                // 跳过该函数，不做扁平化
                continue;
            }

            if let Err(err) = do_handle(module, function, self.demote_switch) {
                warn!("(flatten) function {:?} failed: {}", function.get_name(), err);
                continue;
            }

            if self.fix_stack {
                unsafe {
                    fix_stack(function.as_value_ref() as *mut std::ffi::c_void);
                }
            }

            if verify_function(function.as_value_ref() as *mut std::ffi::c_void) {
                warn!("(flatten) function {:?} verify failed", function.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

fn do_handle(module: &mut Module<'_>, function: FunctionValue, demote_switch: bool) -> anyhow::Result<()> {
    let Some(entry_block) = get_basic_block_entry(function) else {
        return Err(anyhow::anyhow!(
            "(flatten) function {:?} has no entry block",
            function.get_name()
        ));
    };
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
        .map(|(v, bb)| (v, unsafe { BasicBlock::new(*bb) }))
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

    builder.position_before(&entry_terminator);
    let dispatch_id_ptr = builder.build_alloca(i32_ty, "")?;
    builder.build_store(dispatch_id_ptr, i32_ty.const_int(first_block_dispatch_id as u64, false))?;
    builder.build_unconditional_branch(dispatcher)?;

    entry_terminator.erase_from_basic_block();

    builder.position_at_end(dispatcher);
    let dispatch_id = builder.build_load(i32_ty, dispatch_id_ptr, "")?;
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
                        conditional_br.push(terminator);
                    } else {
                        unconditional_br.push(terminator);
                    }
                },
                InstructionOpcode::Switch => {
                    switch.push(terminator);
                },
                _ => continue, // 其他类型的终结指令不处理
            }
        } else {
            warn!("(flatten) basic block {:?} has no terminator", bb);
        }
    }

    for terminator in unconditional_br {
        let successor_block = branch_inst::get_successor(terminator, 0).unwrap().right().unwrap();
        let dispatch_id = basic_block_mapping[&successor_block.as_mut_ptr()];
        let dispatch_id = i32_ty.const_int(dispatch_id as u64, false);
        builder.position_before(&terminator);
        builder.build_store(dispatch_id_ptr, dispatch_id)?;
        builder.build_unconditional_branch(dispatcher)?;
        terminator.erase_from_basic_block();
    }

    for terminator in conditional_br {
        let successor_true = branch_inst::get_successor(terminator, 0).unwrap().right().unwrap();
        let successor_false = branch_inst::get_successor(terminator, 1).unwrap().right().unwrap();
        let dispatch_id_true = basic_block_mapping[&successor_true.as_mut_ptr()];
        let dispatch_id_false = basic_block_mapping[&successor_false.as_mut_ptr()];
        let dispatch_id_true = i32_ty.const_int(dispatch_id_true as u64, false);
        let dispatch_id_false = i32_ty.const_int(dispatch_id_false as u64, false);
        let cond = terminator.get_operand(0).unwrap().left().unwrap().into_int_value();
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

fn split_entry_block_for_flatten<'a>(
    function: FunctionValue<'a>,
    entry_block: BasicBlock<'a>,
    basic_blocks: &mut Vec<BasicBlock<'a>>,
) -> anyhow::Result<bool> {
    let Some(entry_terminator) = entry_block.get_terminator() else {
        // 没有终结指令，居然还能通过上一层的基本块数量大于2的校验？！
        // 估计是别的Pass干的好事！
        return Ok(false);
    };

    // 调试一下入口块的终结指令
    //debug!("terminator: {:?}", entry_terminator);

    // 计算入口块指令数（用于决定 split 位置）
    let entry_block_inst_count = entry_block.get_instructions().count();

    let mut first_basic_block = None;
    match entry_terminator.get_opcode() {
        InstructionOpcode::Br => {
            if entry_terminator.is_conditional() || entry_terminator.get_num_operands() > 1 {
                // 分裂，让新块只承载 terminator，便于作为起始节点
                let mut split_pos = entry_terminator;
                if entry_block_inst_count > 0 {
                    split_pos = split_pos.get_previous_instruction().unwrap();
                }
                let Some(new_block) = split_basic_block(entry_block, split_pos, ".no.conditional.br", false) else {
                    panic!("failed to split basic block");
                };
                if new_block.get_parent().unwrap() != function {
                    return Err(anyhow!("Split block has wrong parent"));
                }
                first_basic_block = new_block.into();
            } else {
                // 无条件跳转，直接取目标块为第一个实际执行的块
                first_basic_block = entry_terminator
                    .get_operand(0)
                    .ok_or(anyhow!("expected operand for unconditional br"))?
                    .right()
                    .ok_or(anyhow!("expected right operand for unconditional br"))?
                    .into();
            }
        },
        InstructionOpcode::Switch => {
            // 这些 terminator 没有 单一落地块 概念，为保持与 br的 一致的处理，
            // 分裂出仅包含 terminator 的新块作为 first_basic_block
            let mut split_pos = entry_terminator;
            if entry_block_inst_count > 0 {
                split_pos = split_pos.get_previous_instruction().unwrap();
            }
            let Some(new_block) = split_basic_block(entry_block, split_pos, ".no.conditional.term", false) else {
                panic!("failed to split basic block");
            };
            if new_block.get_parent().unwrap() != function {
                return Err(anyhow!("Split block has wrong parent"));
            }
            first_basic_block = new_block.into();
        },
        InstructionOpcode::Return | InstructionOpcode::Unreachable => {
            // 无后继，不需要做 flatten
            return Ok(false);
        },
        _ => return Ok(false),
    }

    let Some(first_basic_block) = first_basic_block.take() else {
        return Err(anyhow::anyhow!("failed to get first basic block: {entry_terminator}"));
    };
    if !basic_blocks.contains(&first_basic_block) {
        basic_blocks.insert(0, first_basic_block);
    }

    Ok(true)
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
        mapping.insert(bb.as_mut_ptr(), unique);
    }

    mapping
}

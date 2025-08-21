use crate::aotu::flatten::{Flatten, split_entry_block_for_flatten};
use crate::aotu::lower_switch::demote_switch_to_if;
use crate::ptr_type;
use amice_llvm::analysis::dominators::DominatorTree;
use amice_llvm::ir::basic_block::get_first_insertion_pt;
use amice_llvm::ir::branch_inst::get_successor;
use amice_llvm::ir::function::{fix_stack, get_basic_block_entry};
use amice_llvm::ir::switch_inst::find_case_dest;
use amice_llvm::module_utils::{VerifyResult, append_to_compiler_used, verify_function};
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::BasicType;
use llvm_plugin::inkwell::values::{AsValueRef, BasicValue, FunctionValue, InstructionOpcode};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use log::{info, warn};
use rand::Rng;
use rand::prelude::SliceRandom;
use std::collections::HashMap;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};

pub(crate) fn run(pass: &Flatten, module: &mut Module<'_>) -> anyhow::Result<()> {
    let update_key_fn = build_update_key_function(module, pass.inline_fn)?;

    'out: for function in module.get_functions() {
        if function.count_basic_blocks() <= 2 {
            continue;
        }

        if function == update_key_fn {
            continue;
        }

        if pass.skip_big_function && function.count_basic_blocks() > 4096 {
            continue;
        }

        for _ in 0..pass.loop_count {
            if let Err(err) = do_handle(module, function, update_key_fn, pass.fix_stack) {
                warn!("(flatten-enhanced) function {:?} failed: {}", function.get_name(), err);
                continue 'out;
            }

            if pass.skip_big_function && function.count_basic_blocks() > 4096 {
                break;
            }
        }

        if let VerifyResult::Broken(e) = verify_function(function) {
            warn!(
                "(flatten-enhanced) function {:?} verify failed: {}",
                function.get_name(),
                e
            );
        }
    }

    Ok(())
}

fn do_handle(
    module: &mut Module<'_>,
    function: FunctionValue,
    update_key_fn: FunctionValue,
    fix_stack: bool,
) -> anyhow::Result<()> {
    let Some(entry_block) = get_basic_block_entry(function) else {
        return Err(anyhow::anyhow!(
            "(flatten-enhanced) function {:?} has no entry block",
            function.get_name()
        ));
    };

    if function.count_basic_blocks() <= 2 {
        warn!(
            "(flatten-enhanced) function {:?} has only {} basic blocks, skipping",
            function.get_name(),
            function.count_basic_blocks()
        );
        return Ok(());
    }

    let mut has_eh_or_invoke_in_entry = false;
    for inst in entry_block.get_instructions() {
        match inst.get_opcode() {
            InstructionOpcode::Invoke
            | InstructionOpcode::LandingPad
            | InstructionOpcode::CatchSwitch
            | InstructionOpcode::CatchPad
            | InstructionOpcode::CatchRet
            | InstructionOpcode::CleanupPad
            | InstructionOpcode::CallBr => {
                has_eh_or_invoke_in_entry = true;
                break;
            },
            _ => {},
        }
    }
    if has_eh_or_invoke_in_entry {
        // 跳过该函数，不做扁平化
        warn!(
            "(flatten-enhanced) function {:?} has exception handling or invoke in entry block, skipping",
            function.get_name()
        );
        return Ok(());
    }

    {
        // 执行switch降级，避免奇怪的分析
        let switch_inst_list = function
            .get_basic_blocks()
            .into_iter()
            .filter_map(|bb| bb.get_terminator())
            .filter(|inst| inst.get_opcode() == InstructionOpcode::Switch)
            .collect::<Vec<_>>();

        if !switch_inst_list.is_empty() {
            for inst in switch_inst_list {
                demote_switch_to_if(module, function, inst, false)?;
            }
        }
    }

    let mut basic_blocks = function.get_basic_blocks();
    basic_blocks.retain(|bb| bb != &entry_block); // 去除入口块
    if !split_entry_block_for_flatten(function, entry_block, &mut basic_blocks)? {
        // 切割失败，未知的终结指令！？或者是可忽略的
        // 这并不是错误，是可预期的！
        return Ok(());
    }

    // 每个块自己的密钥，用于更新key array
    let mut block_key_map = HashMap::<BasicBlock, u64>::new();
    // 子块的最终密钥结果，如果程序密钥按正确的路径执行也就是被篡改了，运行时的密钥和这里保存的就会不一致
    let mut block_valid_key_map = HashMap::<BasicBlock, u64>::new();
    // 分发块使用的唯一数字ID
    let mut block_magic_map = HashMap::<BasicBlock, u64>::new();
    let mut basic_block_index_map = HashMap::<BasicBlock, usize>::new();
    {
        // 生成基本块的唯一标识符
        let mut rng = rand::rng();
        let mut values = Vec::with_capacity(basic_blocks.len());
        for bb in &basic_blocks {
            let mut unique = rng.random::<u64>();
            while values.contains(&unique) {
                unique = rng.random();
            }
            values.push(unique);
            block_magic_map.insert(*bb, unique);
        }

        assert_eq!(values.len(), basic_blocks.len());

        values.shuffle(&mut rng); // 打乱！
        for (index, bb) in basic_blocks.iter().enumerate() {
            block_key_map.insert(*bb, values[index]);
            block_valid_key_map.insert(*bb, 0);
            basic_block_index_map.insert(*bb, index);
        }
    }

    let ctx = module.get_context();
    let i8_type = ctx.i8_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_type = ctx.i32_type();
    let i64_type = ctx.i64_type();
    let ptr_type = ctx.ptr_type(AddressSpace::default());

    let i8_zero = i8_type.const_zero();
    let i8_one = i8_type.const_int(1, false);
    let i32_zero = i32_type.const_zero();
    let i32_one = i32_type.const_int(1, false);

    let builder = ctx.create_builder();

    let block_count = i32_type.const_int(basic_blocks.len() as u64, false);

    let first_insertion_pt = get_first_insertion_pt(entry_block);
    builder.position_before(&first_insertion_pt);
    let visited_array = builder.build_array_alloca(i8_type, block_count, "visited")?;
    let key_array = builder.build_array_alloca(i64_type, block_count, "key_array")?;
    builder.build_memset(visited_array, 1, i8_zero, block_count)?;
    let key_ptr = builder.build_bit_cast(key_array, i8_ptr, "key_ptr")?;
    builder.build_memset(
        key_ptr.into_pointer_value(),
        8,
        i8_zero,
        block_count.const_mul(i64_type.size_of()),
    )?;

    let dominators = DominatorTree::from_function(function)
        .map_err(|err| anyhow::anyhow!("failed to build dominator tree: {}", err))?;
    for bb in &basic_blocks {
        let mut dominator_blocks = Vec::new();
        for child in &basic_blocks {
            if *bb != *child && dominators.dominate(*bb, *child) {
                dominator_blocks.push(*child);
                let new_key = block_valid_key_map[child] ^ block_key_map[bb];
                block_valid_key_map.insert(*child, new_key);
            }
        }

        let Some(terminator) = bb.get_terminator() else {
            return Err(anyhow::anyhow!("block {:?} has no terminator", bb.get_name()));
        };
        builder.position_before(&terminator);

        let current_block_index = i32_type.const_int(basic_block_index_map[bb] as u64, false);
        if !dominator_blocks.is_empty() {
            let dominator_count = i32_type.const_int(dominator_blocks.len() as u64, false);
            let dominator_index_array = dominator_blocks
                .iter()
                .map(|bb| basic_block_index_map[bb])
                .map(|index| i32_type.const_int(index as u64, false))
                .collect::<Vec<_>>();
            let dominator_index_array = i32_type.const_array(&dominator_index_array);
            let global_dominator_index_array = module.add_global(dominator_index_array.get_type(), None, "");
            global_dominator_index_array.set_linkage(Linkage::Private);
            global_dominator_index_array.set_constant(true);
            global_dominator_index_array.set_initializer(&dominator_index_array);

            // void update_key_arr(i32* dom_index_arr, i32 dom_index_arr_size, i64 *key_arr, i64 key, i8* visited_arr, i32 current_block_index)
            let args = [
                global_dominator_index_array.as_basic_value_enum(),
                dominator_count.as_basic_value_enum(),
                key_array.as_basic_value_enum(),
                i64_type.const_int(block_key_map[bb], false).as_basic_value_enum(),
                visited_array.as_basic_value_enum(),
                current_block_index.as_basic_value_enum(),
            ]
            .map(|arg| arg.into());
            builder.build_call(update_key_fn, &args, "")?;
        } else {
            let visited_gep =
                unsafe { builder.build_in_bounds_gep(i8_type, visited_array, &[current_block_index], "") }?;
            builder.build_store(visited_gep, i8_one)?;
        }
    }

    let bb_dispatcher = ctx.append_basic_block(function, "bb.dispatcher");
    let bb_dispatcher_default = ctx.append_basic_block(function, "bb.dispatcher.default");

    bb_dispatcher
        .move_after(entry_block)
        .map_err(|_| anyhow::anyhow!("failed to move dispatcher block after entry block"))?;
    bb_dispatcher_default
        .move_after(bb_dispatcher)
        .map_err(|_| anyhow::anyhow!("failed to move dispatcher default block after dispatcher block"))?;

    let dispatcher_entry = basic_blocks[0];
    let dispatcher_entry_id = block_magic_map[&dispatcher_entry];

    let Some(terminator) = entry_block.get_terminator() else {
        return Err(anyhow::anyhow!(
            "block {:?} has no terminator",
            dispatcher_entry.get_name()
        ));
    };
    builder.position_before(&terminator);
    let start_dispatch_id = i64_type.const_int(dispatcher_entry_id, false);
    let dispatch_id = builder.build_alloca(i64_type, "dispatch_id")?;
    builder.build_store(dispatch_id, start_dispatch_id)?;
    builder.build_unconditional_branch(bb_dispatcher)?;
    terminator.erase_from_basic_block();

    builder.position_at_end(bb_dispatcher);
    let cases = block_magic_map
        .iter()
        .map(|(bb, magic)| (i64_type.const_int(*magic, false), *bb))
        .collect::<Vec<_>>();
    let dispatch_id_val = builder
        .build_load(i64_type, dispatch_id, "dispatch_id")?
        .into_int_value();
    let switch = builder.build_switch(dispatch_id_val, bb_dispatcher_default, &cases)?;

    for bb in basic_blocks {
        let Some(terminator) = bb.get_terminator() else {
            return Err(anyhow::anyhow!("block {:?} has no terminator", bb.get_name()));
        };
        if terminator.get_opcode() != InstructionOpcode::Br {
            continue;
        }

        builder.position_before(&terminator);

        if terminator.get_num_operands() == 1 {
            let successor = get_successor(terminator, 0)
                .ok_or_else(|| anyhow::anyhow!("failed to get successor for terminator {:?}", terminator))?;
            let Some(dispatch_id_val) = find_case_dest(switch, successor) else {
                return Err(anyhow::anyhow!(
                    "failed to find case destination for block {:?}, switch: {:?}, successor: {:?}",
                    bb.get_name(),
                    switch,
                    successor
                ));
            };
            let dispatch_id_val = dispatch_id_val.into_int_value().get_zero_extended_constant().unwrap();
            let encrypted_dispatch_id = dispatch_id_val ^ block_valid_key_map[&bb];
            let encrypted_dispatch_id = i64_type.const_int(encrypted_dispatch_id, fix_stack);
            let key_gep = unsafe {
                builder.build_in_bounds_gep(
                    i64_type,
                    key_array,
                    &[i32_type.const_int(basic_block_index_map[&bb] as u64, false)],
                    "",
                )
            }?;
            let key = builder.build_load(i64_type, key_gep, "")?.into_int_value();
            let dispatch_id_val = builder.build_xor(key, encrypted_dispatch_id, "dispatch_id")?;
            builder.build_store(dispatch_id, dispatch_id_val)?;
            builder.build_unconditional_branch(bb_dispatcher)?;
            terminator.erase_from_basic_block();
        } else {
            let true_successor = get_successor(terminator, 0)
                .ok_or_else(|| anyhow::anyhow!("failed to get successor for terminator {:?}", terminator))?;
            let false_successor = get_successor(terminator, 1)
                .ok_or_else(|| anyhow::anyhow!("failed to get successor for terminator {:?}", terminator))?;
            let Some(true_dispatch_id_val) = find_case_dest(switch, true_successor) else {
                return Err(anyhow::anyhow!(
                    "failed to find case destination for block {:?}, switch: {:?}, successor0: {:?}, successor1: {:?}",
                    bb.get_name(),
                    switch,
                    true_successor,
                    false_successor
                ));
            };
            let Some(false_dispatch_id_val) = find_case_dest(switch, false_successor) else {
                return Err(anyhow::anyhow!(
                    "failed to find case destination for block {:?}, switch: {:?}, successor1: {:?}, successor0: {:?}",
                    bb.get_name(),
                    switch,
                    false_successor,
                    true_successor
                ));
            };
            let true_dispatch_id_val = true_dispatch_id_val.into_int_value().get_zero_extended_constant().unwrap();
            let false_dispatch_id_val = false_dispatch_id_val.into_int_value().get_zero_extended_constant().unwrap();
            let encrypted_true_dispatch_id = true_dispatch_id_val ^ block_valid_key_map[&bb];
            let encrypted_false_dispatch_id = false_dispatch_id_val ^ block_valid_key_map[&bb];
            let encrypted_true_dispatch_id = i64_type.const_int(encrypted_true_dispatch_id, fix_stack);
            let encrypted_false_dispatch_id = i64_type.const_int(encrypted_false_dispatch_id, fix_stack);
            let key_gep = unsafe {
                builder.build_in_bounds_gep(
                    i64_type,
                    key_array,
                    &[i32_type.const_int(basic_block_index_map[&bb] as u64, false)],
                    "",
                )
            }?;
            let key = builder.build_load(i64_type, key_gep, "")?.into_int_value();
            let cond = terminator.get_operand(0).unwrap().left().unwrap().into_int_value();
            let dest_dispatch_id = builder.build_select(cond, encrypted_true_dispatch_id, encrypted_false_dispatch_id, "dispatch_id")?
                .into_int_value();
            let dispatch_id_val = builder.build_xor(key, dest_dispatch_id, "dispatch_id")?;
            builder.build_store(dispatch_id, dispatch_id_val)?;
            builder.build_unconditional_branch(bb_dispatcher)?;
            terminator.erase_from_basic_block();
        }
    }

    builder.position_at_end(bb_dispatcher_default);
    builder.build_unconditional_branch(bb_dispatcher)?;

    if fix_stack {
        unsafe { amice_llvm::ir::function::fix_stack(function); }
    }

    Ok(())
}

fn build_update_key_function<'a>(module: &mut Module<'a>, inline_fn: bool) -> anyhow::Result<FunctionValue<'a>> {
    let ctx = module.get_context();

    let i8_type = ctx.i8_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_type = ctx.i32_type();
    let i32_ptr = ptr_type!(ctx, i32_type);
    let i64_type = ctx.i64_type();
    let i64_ptr = ptr_type!(ctx, i64_type);

    let i8_zero = i8_type.const_zero();
    let i8_one = i8_type.const_int(1, false);
    let i32_zero = i32_type.const_zero();
    let i32_one = i32_type.const_int(1, false);

    let builder = ctx.create_builder();

    // void update_key_arr(i32* dom_index_arr, i32 dom_index_arr_size, i64 *key_arr, i64 key, i8* visited_arr, i32 current_block_index)
    let fn_type = ctx.void_type().fn_type(
        &[
            i32_ptr.into(),
            i32_type.into(),
            i64_ptr.into(),
            i64_type.into(),
            i8_ptr.into(),
            i32_type.into(),
        ],
        false,
    );
    let update_fn = module.add_function("update_key_arr", fn_type, None);

    if inline_fn {
        let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        update_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
    }


    let bb_update_key_arr_entry = ctx.append_basic_block(update_fn, "update_fn.entry");
    let bb_update_key_arr_cond = ctx.append_basic_block(update_fn, "update_fn.for.cond");
    let bb_update_key_arr_body = ctx.append_basic_block(update_fn, "update_fn.for.body");
    let bb_update_key_arr_inc = ctx.append_basic_block(update_fn, "update_fn.for.inc");
    let bb_update_key_arr_end = ctx.append_basic_block(update_fn, "update_fn.for.end");
    let bb_update_key_arr_ret = ctx.append_basic_block(update_fn, "update_fn.ret");

    let dom_index_arr = update_fn
        .get_nth_param(0)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get dom_index_arr parameter"))?;
    let dominator_index_array_size = update_fn
        .get_nth_param(1)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow!("Failed to get dom_index_arr_size parameter"))?;
    let key_array = update_fn
        .get_nth_param(2)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get key_arr parameter"))?;
    let block_key = update_fn
        .get_nth_param(3)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow!("Failed to get key parameter"))?;
    let visited_array = update_fn
        .get_nth_param(4)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get visited_arr parameter"))?;
    let current_block_index = update_fn
        .get_nth_param(5)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow!("Failed to get current_block_index parameter"))?;

    builder.position_at_end(bb_update_key_arr_entry);
    let visited_gep = unsafe { builder.build_in_bounds_gep(i8_type, visited_array, &[current_block_index], "") }?;
    let visited = builder.build_load(i8_type, visited_gep, "visited")?.into_int_value();
    let index = builder.build_alloca(i32_type, "index")?;
    builder.build_store(index, i32_zero)?;
    let cond = builder.build_int_compare(IntPredicate::EQ, visited, i8_zero, "visited_cond")?;
    builder.build_conditional_branch(cond, bb_update_key_arr_cond, bb_update_key_arr_ret)?;

    builder.position_at_end(bb_update_key_arr_cond);
    let index_val = builder.build_load(i32_type, index, "loop_i")?.into_int_value();
    let cond = builder.build_int_compare(IntPredicate::SLT, index_val, dominator_index_array_size, "loop_cond")?; // dom_index < dom_size
    builder.build_conditional_branch(cond, bb_update_key_arr_body, bb_update_key_arr_end)?; // if cond goto bb_update_key_arr else goto bb_update_key_arr_end

    builder.position_at_end(bb_update_key_arr_body);
    let index_val = builder.build_load(i32_type, index, "loop_i")?.into_int_value();
    let dom_index_gep_ptr = unsafe { builder.build_in_bounds_gep(i32_type, dom_index_arr, &[index_val], "") }?;
    let dom_block_index = builder
        .build_load(i32_type, dom_index_gep_ptr, "dom_block_index")?
        .into_int_value();
    let dom_key_gep_ptr = unsafe { builder.build_in_bounds_gep(i64_type, key_array, &[dom_block_index], "") }?;
    let dom_key_val = builder
        .build_load(i64_type, dom_key_gep_ptr, "dom_key_val")?
        .into_int_value();
    let updated_key = builder.build_xor(dom_key_val, block_key, "updated_key")?; // new_key = dom_key ^ current_key
    builder.build_store(dom_key_gep_ptr, updated_key)?; // key_array[i] = new_key
    builder.build_unconditional_branch(bb_update_key_arr_inc)?;

    builder.position_at_end(bb_update_key_arr_inc);
    let index_val = builder.build_load(i32_type, index, "loop_i")?.into_int_value();
    let new_index = builder.build_int_nsw_add(index_val, i32_one, "")?;
    builder.build_store(index, new_index)?; // loop_i++
    builder.build_unconditional_branch(bb_update_key_arr_cond)?;

    builder.position_at_end(bb_update_key_arr_end);
    let visited_gep = unsafe { builder.build_in_bounds_gep(i8_type, visited_array, &[current_block_index], "") }?;
    builder.build_store(visited_gep, i8_one)?;
    builder.build_unconditional_branch(bb_update_key_arr_ret)?;

    builder.position_at_end(bb_update_key_arr_ret);
    builder.build_return(None)?;

    if let VerifyResult::Broken(e) = verify_function(update_fn) {
        warn!(
            "(flatten-enhanced) function {:?} verify failed: {}",
            update_fn.get_name(),
            e
        );
    }

    Ok(update_fn)
}

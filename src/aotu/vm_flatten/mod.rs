use crate::llvm_utils::basic_block::split_basic_block;
use crate::llvm_utils::function::get_basic_block_entry;
use crate::ptr_type;
use amice_llvm::ir::function::{fix_stack, fix_stack_at_terminator};
use anyhow::anyhow;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::BasicType;
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, FunctionValue, InstructionOpcode, IntValue};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, info, warn};
use rand::Rng;
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;
use llvm_plugin::inkwell::llvm_sys::core::LLVMGetSwitchDefaultDest;
use crate::llvm_utils::branch_inst::get_successor;
use crate::llvm_utils::switch_inst;

const MAGIC_NUMBER: u32 = 0x7788ff;

pub struct VmFlatten {
    enable: bool,
    random_none_node_opcode: bool,
}

impl LlvmModulePass for VmFlatten {
    fn run_pass(
        &self,
        module: &mut Module<'_>,
        manager: &ModuleAnalysisManager,
    ) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        for function in module.get_functions() {
            if let Err(err) = do_handle(self, module, function) {
                error!(
                    "(vm_flatten) failed to handle function: {}, err = {}",
                    function.get_name().to_str().unwrap_or("<unknown>"),
                    err
                );
            }
        }

        PreservedAnalyses::None
    }
}

#[derive(Debug, Copy, Clone)]
struct VmBrNode<'a> {
    value: u32,
    labels: NonNull<Vec<BasicBlock<'a>>>,
    block: BasicBlock<'a>,
    opcode: InstructionOpcode
}

impl<'a> VmBrNode<'a> {
    fn new(num: u32, block: BasicBlock<'a>) -> Self {
        let labels = Vec::new();
        let labels = Box::new(labels);
        Self {
            value: num,
            labels: NonNull::from(Box::leak(labels)),
            block,
            opcode: InstructionOpcode::Br,
        }
    }

    fn new_unconditional_br(num: u32, block: BasicBlock<'a>, left: BasicBlock<'a>) -> Self {
        let mut labels = Vec::new();
        labels.push(left);
        let labels = Box::new(labels);
        Self {
            value: num,
            labels: NonNull::from(Box::leak(labels)),
            block,
            opcode: InstructionOpcode::Br,
        }
    }

    fn get_left(&self) -> BasicBlock<'a> {
        self.as_slice()[0]
    }

    fn get_right(&self) -> BasicBlock<'a> {
        self.as_slice()[1]
    }

    fn set_left(&mut self, left: BasicBlock<'a>) {
        if self.len() < 1 {
            self.push(left);
            return;
        }
        self.as_slice_mut()[0]= left;
    }

    fn set_right(&mut self, right: BasicBlock<'a>) {
        if self.len() < 1 {
            panic!("set_right: labels.len() < 1");
        }
        if self.len() < 2 {
            self.push(right);
            return;
        }
        self.as_slice_mut()[1] = right;
    }

    fn len(&self) -> usize {
        unsafe {
            (*self.labels.as_ptr()).len()
        }
    }

    fn as_slice_mut(&mut self) -> &mut [BasicBlock<'a>] {
        unsafe { &mut *self.labels.as_ptr() }
    }

    fn as_slice(&self) -> &[BasicBlock<'a>] {
        unsafe { &*self.labels.as_ptr() }
    }

    fn push(&mut self, block: BasicBlock<'a>) {
        unsafe {
            (*self.labels.as_ptr()).push(block);
        }
    }

    fn get_block(&self) -> BasicBlock<'a> {
        self.block
    }

    fn free(&self) {
        unsafe {
            let _ = Box::from_raw(self.labels.as_ptr());
        }
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
enum VmBrNodeKind {
    Jmp = 114,
    JmpIf = 514,
    Run = 1919,
    Switch = 810,
    None = 0,
}

fn do_handle<'a>(
    pass: &VmFlatten,
    module: &mut Module<'a>,
    function: FunctionValue,
) -> anyhow::Result<()> {
    let mut basic_blocks = function.get_basic_blocks();
    if basic_blocks.is_empty() {
        return Ok(());
    }

    if basic_blocks.len() <= 1 {
        return Ok(());
    }

    let Some(entry_block) = get_basic_block_entry(function) else {
        return Err(anyhow::anyhow!("failed to get entry block"));
    };

    let entry_block_inst_count = entry_block.get_instructions().count();

    // 从basic block移除入口基本块
    let entry_ptr = entry_block.as_mut_ptr();
    basic_blocks.retain(|bb| bb.as_mut_ptr() != entry_ptr);

    let mut first_basic_block = None;
    for inst in entry_block.get_instructions() {
        if inst.get_opcode() == InstructionOpcode::Br {
            if inst.is_conditional() || inst.get_num_operands() > 1 {
                let mut split_pos = inst;
                if entry_block_inst_count > 0 {
                    split_pos = split_pos.get_previous_instruction().unwrap();
                }
                let Some(new_block) =
                    split_basic_block(entry_block, split_pos, ".no.conditional.br", false)
                else {
                    panic!("failed to split basic block");
                };
                if new_block.get_parent().unwrap() != function {
                    return Err(anyhow!("Split block has wrong parent"));
                }
                first_basic_block = new_block.into();
            } else {
                first_basic_block = inst
                    .get_operand(0)
                    .ok_or(anyhow!("expected operand for unconditional br"))?
                    .right()
                    .ok_or(anyhow!("expected right operand for unconditional br"))?
                    .into();
            }
            break;
        }
    }
    let Some(first_basic_block) = first_basic_block else {
        return Err(anyhow::anyhow!("failed to get first basic block"));
    };
    if !basic_blocks.contains(&first_basic_block) {
        basic_blocks.insert(0, first_basic_block);
    }

    let mut all_nodes = Vec::new();
    let mut basic_block_value_map = HashMap::new();

    for bb in &basic_blocks {
        let value = generate_unique_value(&all_nodes);
        basic_block_value_map.insert(*bb, value);
        all_nodes.push(VmBrNode::new(value, *bb));
    }

    for node in &mut all_nodes {
        for inst in node.block.get_instructions() {
            if inst.get_opcode() == InstructionOpcode::Br {
                if inst.is_conditional() || inst.get_num_operands() > 1 {
                    let left = get_successor(inst, 0);
                    let right = get_successor(inst, 1);
                    let left = left.ok_or(anyhow!(
                            "expected left operand for conditional br: op_nums > 1"
                        ))?
                        .right()
                        .ok_or(anyhow!(
                            "expected left operand for conditional br: is not a block"
                        ))?;
                    let right = right.ok_or(anyhow!(
                            "expected right operand for conditional br: op_nums > 1"
                        ))?
                        .right()
                        .ok_or(anyhow!(
                            "expected right operand for conditional br: is not a block"
                        ))?;

                    node.set_left(left.into());
                    node.set_right(right.into());
                } else {
                    let left = inst
                        .get_operand(0)
                        .ok_or(anyhow!(
                            "expected left operand for conditional br: {:?}",
                            inst
                        ))?
                        .right()
                        .ok_or(anyhow!("expected left operand for is not a block"))?;

                    node.set_left(left.into());
                }
                node.opcode = InstructionOpcode::Br;
                break;
            }
            else if inst.get_opcode() == InstructionOpcode::Return {
                node.opcode = InstructionOpcode::Return;
                break;
            } else if inst.get_opcode() == InstructionOpcode::Switch {
                // switch i32 %11, label %28 [
                //  i32 0, label %12
                //  i32 1, label %15
                //  i32 2, label %18
                //  ]
                let default_case = switch_inst::get_default_block(inst);
                let condition = switch_inst::get_condition(inst);
                let cases = switch_inst::get_cases(inst);
                node.push(default_case);
                for (_, bb) in cases {
                    node.push(bb);
                }
                node.opcode = InstructionOpcode::Switch;
            }
        }
    }

    let mut opcodes = Vec::new();
    let mut opcode_inst_map = HashMap::new();
    generate_opcodes(
        &all_nodes,
        &basic_block_value_map,
        &mut opcode_inst_map,
        &mut opcodes,
        VmBrNode::new_unconditional_br(MAGIC_NUMBER, first_basic_block, first_basic_block),
        pass.random_none_node_opcode,
    )?;

    debug!(
        "(vm_flatten) fun: {} opcodes: {:?}",
        function.get_name().to_str().unwrap_or("<unknown>"),
        opcodes
    );

    let ctx = module.get_context();
    let i32_type = ctx.i32_type();
    let pty_ty = ptr_type!(ctx, i32_type);
    let i32_one = i32_type.const_int(1, false);
    let i32_two = i32_type.const_int(2, false);
    let i32_three = i32_type.const_int(3, false);
    let i32_zero = i32_type.const_int(0, false);
    let i32_jmp = i32_type.const_int(VmBrNodeKind::Jmp as u64, false);
    let i32_jmp_if = i32_type.const_int(VmBrNodeKind::JmpIf as u64, false);
    let i32_run = i32_type.const_int(VmBrNodeKind::Run as u64, false);
    let i32_none = i32_type.const_int(VmBrNodeKind::None as u64, false);

    let opcode_array_type = i32_type.array_type(opcodes.len() as u32 * 3);
    let mut opcode_llvm_values = Vec::new();
    for x in opcodes {
        let op = i32_type.const_int(x.0 as u64, false);
        let left = i32_type.const_int(x.1 as u64, false);
        let right = i32_type.const_int(x.2 as u64, false);
        opcode_llvm_values.push(op);
        opcode_llvm_values.push(left);
        opcode_llvm_values.push(right);
    }
    let opcode_array =
        unsafe { ArrayValue::new_const_array(&opcode_array_type, &opcode_llvm_values) };

    let local_opcodes_value =
        module.add_global(opcode_array_type, None, ".amice.vm_flatten_opcodes");
    local_opcodes_value.set_constant(false);
    local_opcodes_value.set_initializer(&opcode_array);
    local_opcodes_value.set_linkage(Linkage::Private);
    unsafe {
        amice_llvm::module_utils::append_to_compiler_used(
            module.as_mut_ptr() as *mut std::ffi::c_void,
            local_opcodes_value.as_value_ref() as *mut std::ffi::c_void,
        )
    };

    let builder = ctx.create_builder();
    let vm_entry = ctx.append_basic_block(function, ".amice.vm_flatten_entry");
    let vm_run = ctx.append_basic_block(function, ".amice.vm_flatten_run");
    let vm_jmp_if = ctx.append_basic_block(function, ".amice.vm_flatten_jmp_if");
    let vm_jmp = ctx.append_basic_block(function, ".amice.vm_flatten_jmp");
    let vm_default = ctx.append_basic_block(function, ".amice.vm_flatten_default");

    builder.position_at_end(vm_default);
    builder.build_unconditional_branch(vm_entry)?;

    let Some(entry_term_inst) = entry_block.get_terminator() else {
        return Err(anyhow!("expected entry block to have terminator"));
    };
    builder.position_before(&entry_term_inst);
    let vm_br_flag = builder.build_alloca(i32_type, ".amice.vm_flatten_br_flag")?;
    let pc = builder.build_alloca(i32_type, ".amice.vm_flatten_pc")?;
    builder.build_store(pc, i32_zero)?;
    builder.build_store(vm_br_flag, i32_zero)?;
    let br_to_vm_entry = builder.build_unconditional_branch(vm_entry)?;
    entry_term_inst.replace_all_uses_with(&br_to_vm_entry);
    entry_term_inst.erase_from_basic_block();

    builder.position_at_end(vm_entry);

    let pc_value = builder.build_load(i32_type, pc, "__pc__")?.into_int_value();
    let pc_plus_one = builder.build_int_add(pc_value, i32_one, "pc_plus_1")?;
    let pc_plus_two = builder.build_int_add(pc_value, i32_two, "pc_plus_2")?;
    let pc_plus_three = builder.build_int_add(pc_value, i32_three, "pc_plus_3")?;

    let opcode = builder.build_load(
        i32_type,
        unsafe {
            builder.build_in_bounds_gep(opcode_array_type, local_opcodes_value.as_pointer_value(), &[i32_zero, pc_value], "",)
        }?,
        "__op__"
    )?.into_int_value();
    let left = builder.build_load(
        i32_type,
        unsafe {
            builder.build_in_bounds_gep(opcode_array_type, local_opcodes_value.as_pointer_value(), &[i32_zero, pc_plus_one], "",)
        }?,
        "__left__"
    )?.into_int_value();
    let right = builder.build_load(
        i32_type,
        unsafe {
            builder.build_in_bounds_gep(opcode_array_type, local_opcodes_value.as_pointer_value(), &[i32_zero, pc_plus_two], "",)
        }?,
        "__right__"
    )?.into_int_value();
    builder.build_store(pc, pc_plus_three)?;

    builder.build_switch(
        opcode,
        vm_default,
        &[
            (i32_jmp, vm_jmp),
            (i32_jmp_if, vm_jmp_if),
            (i32_run, vm_run),
            (i32_none, vm_default),
        ],
    )?;

    builder.position_at_end(vm_run);
    let mut cases = Vec::<(IntValue, BasicBlock)>::new();
    for bb in &basic_blocks {
        bb.move_before(vm_default).map_err(|_| anyhow!("move basic block failed"))?;
        let block_value = basic_block_value_map[bb];
        cases.push((i32_type.const_int(block_value as u64, false), *bb));
    }
    builder.build_switch(left, vm_default, &cases)?;

    for bb in basic_blocks {
        let mut has_return = false;
        let branches = bb.get_instructions()
            .filter(|inst| {
                let op = inst.get_opcode();
                op == InstructionOpcode::Br || op == InstructionOpcode::Return
            })
            .collect::<Vec<_>>();
        for inst in branches {
            if inst.get_opcode() == InstructionOpcode::Return {
                has_return = true;
                continue;
            }

            builder.position_before(&inst);
            let new_br = if inst.is_conditional() && inst.get_num_operands() == 3 {
                let result = inst
                    .get_operand(0)
                    .ok_or(anyhow!("inst.get_operand(inst.get_num_operands() - 1)"))?
                    .left()
                    .ok_or(anyhow!("expected left operand is basic value: {:?}", inst))?
                    .into_int_value();
                let flag_value = builder
                    .build_select(result, i32_one, i32_zero, "_t_or_f_2")?
                    .into_int_value();
                builder.build_store(vm_br_flag, flag_value)?;
                builder.build_unconditional_branch(vm_entry)?
            } else if inst.get_num_operands() == 1 {
                builder.build_unconditional_branch(vm_entry)?
            } else {
                continue
            };
            inst.replace_all_uses_with(&new_br);
            inst.erase_from_basic_block();
        }

        if !has_return && bb.get_terminator().is_none() {
            builder.position_at_end(bb);
            builder.build_unconditional_branch(vm_entry)?;
        }
    }

    builder.position_at_end(vm_jmp);
    builder.build_store(pc, left)?;
    builder.build_unconditional_branch(vm_entry)?;

    builder.position_at_end(vm_jmp_if);
    let flag_value = builder.build_load(i32_type, vm_br_flag, "__vm_br_flag__")?.into_int_value();

    let jump_true = ctx.append_basic_block(function, ".amice.jump_true");
    let jump_false = ctx.append_basic_block(function, ".amice.jump_false");

    let jmp_cmp = builder.build_int_compare(IntPredicate::EQ, flag_value, i32_one, "jmp_cmp")?;
    builder.build_conditional_branch(jmp_cmp, jump_true, jump_false)?;

    builder.position_at_end(jump_true);
    builder.build_store(pc, left)?;
    builder.build_unconditional_branch(vm_entry)?;

    builder.position_at_end(jump_false);
    builder.build_store(pc, right)?;
    builder.build_unconditional_branch(vm_entry)?;

    unsafe {
        if amice_llvm::ir::function::verify_function(
            function.as_value_ref() as *mut std::ffi::c_void
        ) {
            warn!(
                "(vm_flatten) function {} verify failed",
                function.get_name().to_str().unwrap_or("<unknown>")
            );
        }
    }

    unsafe {
        fix_stack(function.as_value_ref() as *mut std::ffi::c_void);
    }

    Ok(())
}

fn generate_opcodes(
    nodes: &[VmBrNode<'_>],
    basic_block_value_map: &HashMap<BasicBlock<'_>, u32>,
    run_block_index_map: &mut HashMap<u32, usize>,
    opcodes: &mut Vec<(VmBrNodeKind, u32, u32, u32)>,
    node: VmBrNode<'_>,
    random_none_node_opcode: bool,
) -> anyhow::Result<()> {
    if node.len() == 2 && node.opcode == InstructionOpcode::Br {
        // let Some(left) = node.get_left() else {
        //     return Err(anyhow!("expected left node for br"));
        // };
        // let Some(right) = node.get_right() else {
        //     return Err(anyhow!("expected right node for br"));
        // };
        let left = node.get_left();
        let right = node.get_right();

        let left_value = *basic_block_value_map.get(&left)
            .ok_or(anyhow!("failed to find left node value for basic block: {:?}",left))?;
        let right_value = *basic_block_value_map.get(&right)
            .ok_or(anyhow!("failed to find right node value for basic block: {:?}",right))?;

        opcodes.push((
            VmBrNodeKind::JmpIf,
            0,
            0,
            node.value
        ));
        let jmpif_index = opcodes.len() - 1;

        // 先生成true分支的代码
        if !run_block_index_map.contains_key(&left_value) {
            opcodes.push((VmBrNodeKind::Run, left_value, 0, left_value));
            let left_pc_index = opcodes.len() - 1;
            run_block_index_map.insert(left_value, left_pc_index);

            // 递归处理true分支的后续
            let left_node = *nodes.iter().find(|n| n.block == left)
                .ok_or(anyhow!("failed to find node for block: {:?}", left))?;
            generate_opcodes(
                nodes,
                basic_block_value_map,
                run_block_index_map,
                opcodes,
                left_node,
                random_none_node_opcode,
            )?;
        }

        // 再生成false分支的代码
        if !run_block_index_map.contains_key(&right_value) {
            opcodes.push((VmBrNodeKind::Run, right_value, 0, right_value));
            let right_pc_index = opcodes.len() - 1;
            run_block_index_map.insert(right_value, right_pc_index);

            // 递归处理false分支的后续
            let right_node = *nodes.iter().find(|n| n.block == right)
                .ok_or(anyhow!("failed to find node for block: {:?}", right))?;
            generate_opcodes(
                nodes,
                basic_block_value_map,
                run_block_index_map,
                opcodes,
                right_node,
                random_none_node_opcode,
            )?;
        }

        // 现在填充JmpIf的跳转地址
        let left_pc = (*run_block_index_map.get(&left_value).unwrap() * 3) as u32;
        let right_pc = (*run_block_index_map.get(&right_value).unwrap() * 3) as u32;

        opcodes[jmpif_index].1 = left_pc;   // true分支跳转地址
        opcodes[jmpif_index].2 = right_pc;  // false分支跳转地址

        // opcodes[jmpif_index].1 = right_pc;
        // opcodes[jmpif_index].2 = left_pc;

        return Ok(());
    } else if node.len() == 1 && node.opcode == InstructionOpcode::Br {
        // let Some(left) = node.left else {
        //     return Err(anyhow!("expected left node for br"));
        // };
        let left = node.get_left();
        let left_value = *basic_block_value_map.get(&left).ok_or(anyhow!(
            "failed to find left node value for basic block: {:?}",
            left
        ))?;
        let right_value = if random_none_node_opcode {
            left_value ^ basic_block_value_map.len() as u32
        } else {
            0
        };
        if !run_block_index_map.contains_key(&left_value) {
            opcodes.push((VmBrNodeKind::Run, left_value, 0, node.value));
            run_block_index_map.insert(left_value, opcodes.len() - 1);

            let next_node = *nodes.iter().find(|n| n.block == left)
                .ok_or(anyhow!("failed to find next node for basic block: {:?}",left))?;
            return generate_opcodes(
                nodes,
                basic_block_value_map,
                run_block_index_map,
                opcodes,
                next_node,
                random_none_node_opcode,
            );
        } else {
            let pc = *run_block_index_map.get(&left_value)
                .unwrap() * 3;
            opcodes.push((VmBrNodeKind::Jmp, pc as u32, 0, node.value));
            return Ok(());
        }
    } else if node.opcode == InstructionOpcode::Switch {
        panic!("not implemented");
    }
    return Ok(())
}

fn generate_unique_value(nodes: &[VmBrNode<'_>]) -> u32 {
    let exists: HashSet<u32> = nodes.iter().map(|n| n.value).collect();
    let mut rng = rand::rng();
    loop {
        let candidate = rng.random::<u32>();
        if candidate != MAGIC_NUMBER && !exists.contains(&candidate) {
            return candidate;
        }
    }
}

impl VmFlatten {
    pub fn new(enable: bool) -> Self {
        Self {
            enable,
            random_none_node_opcode: false,
        }
    }
}

use crate::llvm_utils::function::get_basic_block_entry;
use crate::ptr_type;
use bitflags::bitflags;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::AsTypeRef;
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, BasicValue, InstructionOpcode};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};

const INDIRECT_BRANCH_TABLE_NAME: &str = "global_indirect_branch_table";

pub struct IndirectBranch {
    enable: bool,
    flags: IndirectBranchFlags,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct IndirectBranchFlags: u32 {
        const Basic =             0b00000001;
        const DummyBlock =        0b00000010;
        const ChainedDummyBlock = 0b00000110;
    }
}

impl LlvmModulePass for IndirectBranch {
    fn run_pass(
        &self,
        module: &mut Module<'_>,
        manager: &ModuleAnalysisManager,
    ) -> PreservedAnalyses {
        if !self.enable || !self.flags.contains(IndirectBranchFlags::Basic) {
            return PreservedAnalyses::All;
        }

        let ctx = module.get_context();
        let i8_ty = ctx.i8_type();
        let i8_ptr_ty = ptr_type!(ctx, i8_type);
        let i32_ty = ctx.i32_type();
        let const_zero = i32_ty.const_zero();
        let const_one = i32_ty.const_int(1u64, false);
        let ptr_ty = ctx.ptr_type(AddressSpace::default());

        let basic_blocks = collect_basic_block(module);
        if basic_blocks.is_empty() {
            warn!(
                "No basic blocks found in the module, skipping IndirectBranch pass: {}",
                module.get_name().to_str().unwrap_or("unknown")
            );
            return PreservedAnalyses::All;
        }
        let basic_block_array = basic_blocks
            .iter()
            .filter_map(|bb| unsafe { bb.get_address() })
            .map(|addr| addr.as_value_ref())
            .collect::<Vec<_>>();

        let basic_block_array_ty = ptr_ty.array_type(basic_blocks.len() as u32);
        let initializer = unsafe {
            ArrayValue::new_raw_const_array(basic_block_array_ty.as_type_ref(), &basic_block_array)
        };
        let global_indirect_branch_table =
            module.add_global(basic_block_array_ty, None, INDIRECT_BRANCH_TABLE_NAME);
        global_indirect_branch_table.set_initializer(&initializer);
        global_indirect_branch_table.set_linkage(Linkage::Internal);
        global_indirect_branch_table.set_constant(false); // 防止被优化

        unsafe {
            amice_llvm::module_utils::append_to_compiler_used(
                module.as_mut_ptr() as *mut std::ffi::c_void,
                global_indirect_branch_table.as_value_ref() as *mut std::ffi::c_void,
            );
        }

        for fun in module.get_functions() {
            let mut branch_inst_list = Vec::new();
            for bb in fun.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if inst.get_opcode() == InstructionOpcode::Br {
                        branch_inst_list.push(inst);
                    }
                }
            }

            for bi in branch_inst_list {
                // br label %2
                // br i1 %5, label %6, label %7
                let mut future_branches = [None::<BasicBlock>; 2];
                if bi.is_conditional() {
                    future_branches[0] = bi.get_operand(1).unwrap().right(); // true分支
                    future_branches[1] = bi.get_operand(2).unwrap().right();
                } else {
                    future_branches[0] = bi.get_operand(0).unwrap().right(); // true分支
                }

                let future_branches: Vec<_> = future_branches.iter().filter_map(|&bb| bb).collect();

                let future_branches_address = future_branches
                    .iter()
                    .filter_map(|next_basic_block| unsafe { next_basic_block.get_address() })
                    .map(|addr| addr.as_value_ref())
                    .collect::<Vec<_>>();

                if future_branches_address.is_empty() {
                    warn!("(indirect-branch) branch to Meow? future_branches_address.len() < 1!");
                    continue;
                }

                let indirect_branch_table = if bi.is_conditional()
                    || !basic_block_array.contains(&future_branches_address[0])
                {
                    let basic_block_array_ty =
                        ptr_ty.array_type(future_branches_address.len() as u32);
                    let array_values = future_branches_address
                        .iter()
                        .map(|v| unsafe { ArrayValue::new(*v) })
                        .collect::<Vec<_>>();

                    let initializer = basic_block_array_ty.const_array(&array_values);
                    let local_indirect_branch_table =
                        module.add_global(basic_block_array_ty, None, ".amice.indirect_branch");
                    local_indirect_branch_table.set_initializer(&initializer);
                    local_indirect_branch_table.set_linkage(Linkage::Internal);
                    local_indirect_branch_table.set_constant(false);
                    unsafe {
                        amice_llvm::module_utils::append_to_compiler_used(
                            module.as_mut_ptr() as *mut std::ffi::c_void,
                            local_indirect_branch_table.as_value_ref() as *mut std::ffi::c_void,
                        );
                    }
                    Some(local_indirect_branch_table)
                } else {
                    module.get_global(INDIRECT_BRANCH_TABLE_NAME)
                };

                let Some(indirect_branch_table) = indirect_branch_table else {
                    warn!("(indirect-branch) indirect branch table is None?");
                    continue;
                };

                let builder = ctx.create_builder();
                // 如果是 DummyBlock，则创建一个空的基本块作为目标
                let goal_dummy_block = if self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    let block = ctx.append_basic_block(fun, "");
                    builder.position_at_end(block);
                    Some(block)
                } else {
                    builder.position_before(&bi);
                    None
                };
                let index = if bi.is_conditional() {
                    let cond = bi.get_operand(0).unwrap().left().unwrap();
                    builder
                        .build_int_z_extend(cond.into_int_value(), i32_ty, "")
                        .map_err(|e| warn!("(indirect-branch) build_int_z_extend failed: {e}"))
                        .ok()
                } else {
                    basic_block_array
                        .iter()
                        .position(|&x| x == future_branches_address[0])
                        .map(|x| i32_ty.const_int(x as u64, false))
                };
                let Some(index) = index else {
                    warn!("(indirect-branch) index is None, skipping this branch, branch: {bi:?}");
                    continue;
                };
                let Ok(gep) = (unsafe {
                    builder
                        .build_gep(
                            ptr_ty,
                            indirect_branch_table.as_pointer_value(),
                            &[index],
                            "",
                        )
                        .map_err(|e| error!("(indirect-branch) build gep_index failed: {e}"))
                }) else {
                    panic!("(indirect-branch) build gep_index failed, this should never happen");
                };
                let Ok(loaded_address) = builder
                    .build_load(i8_ptr_ty, gep, "IndirectBranchingTargetAddress")
                    .map_err(|e| error!("(indirect-branch) build load failed: {e}"))
                else {
                    panic!("(indirect-branch) build load failed, this should never happen");
                };
                let Ok(indir_br) = builder
                    .build_indirect_branch(loaded_address.as_basic_value_enum(), &future_branches)
                    .map_err(|e| error!("(indirect-branch) build indirect branch failed: {e}"))
                else {
                    panic!(
                        "(indirect-branch) build indirect branch failed, this should never happen"
                    );
                };

                if self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    let max_chain_num =
                        if self.flags.contains(IndirectBranchFlags::ChainedDummyBlock) {
                            13
                        } else {
                            1
                        };
                    let chain_nums = rand::random_range(0..=max_chain_num);
                    let goal_dummy_block = goal_dummy_block.unwrap();

                    let mut cur_dummy_block = goal_dummy_block;
                    for _ in 0..chain_nums - 1 {
                        let dummy_block = ctx.append_basic_block(fun, "");
                        builder.position_at_end(dummy_block);
                        let target =
                            unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };

                        if rand::random_range(0..=100) < 45 {
                            let dummy_val1 = i32_ty.const_int(rand::random::<u32>() as u64, false);
                            let dummy_val2 = i32_ty.const_int(rand::random::<u32>() as u64, false);
                            if let Ok(alloca) = builder.build_alloca(i32_ty, "junk_volatile") {
                                let _junk_add =
                                    builder.build_int_add(dummy_val1, dummy_val2, "junk");
                                if rand::random::<bool>() {
                                    let _junk_cmp = builder.build_int_compare(
                                        IntPredicate::EQ,
                                        dummy_val1,
                                        dummy_val2,
                                        "junk_cmp",
                                    );
                                }
                                if rand::random_range(0..=100) < 30 {
                                    if let Ok(junk_cmp) = builder.build_int_compare(
                                        IntPredicate::NE,
                                        dummy_val1,
                                        dummy_val2,
                                        "junk_cmp",
                                    ) {
                                        let _ = builder
                                            .build_int_z_extend(junk_cmp, i32_ty, "junk_cmp_zext")
                                            .map(|dummy_val| builder.build_store(alloca, dummy_val))
                                            .map(|result| {
                                                result
                                                    .map(|store_inst| store_inst.set_volatile(true))
                                            });
                                    }
                                }
                            }
                        }

                        builder
                            .build_indirect_branch(target, &[cur_dummy_block])
                            .map_err(|e| {
                                error!("(indirect-branch) build indirect branch failed: {e}")
                            })
                            .expect("build indirect branch failed");
                        cur_dummy_block = dummy_block;
                    }

                    builder.position_before(&bi);
                    let target =
                        unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };
                    builder
                        .build_indirect_branch(target, &[cur_dummy_block])
                        .map_err(|e| error!("(indirect-branch) build_indirect_branch failed: {e}"))
                        .expect("build_indirect_branch failed");
                }

                if !self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    bi.replace_all_uses_with(&indir_br);
                }
                bi.remove_from_basic_block();
            }
        }

        PreservedAnalyses::None
    }
}

fn collect_basic_block<'a>(module: &Module<'a>) -> Vec<BasicBlock<'a>> {
    let mut basic_blocks = Vec::new();
    for fun in module.get_functions() {
        let entry_block = get_basic_block_entry(&fun);
        for bb in fun.get_basic_blocks() {
            if bb.as_mut_ptr() == entry_block {
                continue;
            }
            basic_blocks.push(bb);
        }
    }

    basic_blocks
}

impl IndirectBranch {
    pub fn new(enable: bool) -> Self {
        let mut flags = IndirectBranchFlags::Basic;
        let indirect_flags_str =
            std::env::var("AMICE_INDIRECT_BRANCH_FLAGS").unwrap_or_else(|_| "".to_string());
        for x in indirect_flags_str.split(",") {
            if x.is_empty() {
                continue;
            }
            match x.to_lowercase().as_str() {
                "dummy_block" => flags |= IndirectBranchFlags::DummyBlock,
                "chained_dummy_blocks" => flags |= IndirectBranchFlags::ChainedDummyBlock,
                _ => warn!("Unknown AMICE_INDIRECT_BRANCH_FLAGS: \"{x}\", ignoring"),
            }
        }

        if enable {
            debug!("IndirectBranch pass enabled with flags: {flags:?}");
        }

        Self { enable, flags }
    }
}

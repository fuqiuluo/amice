use crate::config::{Config, IndirectBranchFlags};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, FunctionExt, InstructionExt, ModuleExt};
use amice_llvm::ptr_type;
use amice_macro::amice;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::builder::Builder;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::{AsTypeRef, IntType};
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, BasicValue, InstructionOpcode};
use llvm_plugin::inkwell::{AddressSpace, IntPredicate};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use log::{debug, error, warn};
use rand::Rng;

const INDIRECT_BRANCH_TABLE_NAME: &str = "global_indirect_branch_table";

#[amice(priority = 800, name = "IndirectBranch", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct IndirectBranch {
    enable: bool,
    flags: IndirectBranchFlags,
    xor_key: Option<[u32; 4]>,
}

impl AmicePassLoadable for IndirectBranch {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        self.enable = cfg.indirect_branch.enable;
        self.flags = IndirectBranchFlags::Basic;
        self.flags |= cfg.indirect_branch.flags;

        if self.enable {
            debug!("IndirectBranch pass enabled with flags: {:?}", self.flags);
        }

        self.xor_key = if self.flags.contains(IndirectBranchFlags::EncryptBlockIndex) {
            let mut xor_key = [0u32; 4];
            rand::rng().fill(&mut xor_key[..]);
            xor_key.into()
        } else {
            None
        };

        self.enable
    }
}

impl LlvmModulePass for IndirectBranch {
    fn run_pass(&self, module: &mut Module<'_>, _manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable || !self.flags.contains(IndirectBranchFlags::Basic) {
            return PreservedAnalyses::All;
        }

        let ctx = module.get_context();
        let i32_type = ctx.i32_type();
        let const_zero = i32_type.const_zero();
        let ptr_type = ptr_type!(ctx, i8_type);

        let non_entry_basic_blocks = collect_basic_block(module);
        if non_entry_basic_blocks.is_empty() {
            warn!(
                "No basic blocks found in the module, skipping IndirectBranch pass: {}",
                module.get_name().to_str().unwrap_or("unknown")
            );
            return PreservedAnalyses::All;
        }
        let non_entry_bb_addrs = non_entry_basic_blocks
            .iter()
            .filter_map(|bb| unsafe { bb.get_address() })
            .map(|addr| addr.as_value_ref())
            .collect::<Vec<_>>();

        let non_entry_bb_array_ty = ptr_type.array_type(non_entry_basic_blocks.len() as u32);
        let non_entry_bb_initializer =
            unsafe { ArrayValue::new_raw_const_array(non_entry_bb_array_ty.as_type_ref(), &non_entry_bb_addrs) };
        let global_indirect_branch_table = module.add_global(non_entry_bb_array_ty, None, INDIRECT_BRANCH_TABLE_NAME);
        global_indirect_branch_table.set_initializer(&non_entry_bb_initializer);
        global_indirect_branch_table.set_linkage(Linkage::Internal);
        global_indirect_branch_table.set_constant(true);

        module.append_to_compiler_used(global_indirect_branch_table);

        let encrypt_key_global = if self.flags.contains(IndirectBranchFlags::EncryptBlockIndex) {
            let xor_key = self
                .xor_key
                .as_ref()
                .unwrap()
                .iter()
                .map(|x| i32_type.const_int(*x as u64, false))
                .map(|addr| addr.as_value_ref())
                .collect::<Vec<_>>();
            let xor_key_array_ty = i32_type.array_type(xor_key.len() as u32);
            let initializer = unsafe { ArrayValue::new_raw_const_array(xor_key_array_ty.as_type_ref(), &xor_key) };
            let table = module.add_global(xor_key_array_ty, None, ".amice.indirect_branch_key");
            table.set_initializer(&initializer);
            table.set_linkage(Linkage::Private);
            table.set_constant(true);

            module.append_to_compiler_used(table);

            Some(table)
        } else {
            None
        };

        for function in module.get_functions() {
            let mut branch_instructions = Vec::new();
            for basic_block in function.get_basic_blocks() {
                for instruction in basic_block.get_instructions() {
                    if instruction.get_opcode() == InstructionOpcode::Br {
                        branch_instructions.push(instruction.into_branch_inst());
                    }
                }
            }

            for br_inst in branch_instructions {
                // br label %2
                // br i1 %5, label %6, label %7
                let mut future_branches = [None::<BasicBlock>; 2];
                if br_inst.is_conditional() {
                    // future_branches的[1]是内存下标，get_successor(0)为真块
                    // 当为真时，把bool扩展为i32,则这个i32的值是1，直接作为下标使用即future_branches[1]应该保存真分支
                    future_branches[1] = br_inst.get_successor(0);
                    future_branches[0] = br_inst.get_successor(1);
                } else {
                    future_branches[0] = br_inst.get_successor(0); // true分支
                }

                // 可能要去到的分支
                let successors: Vec<_> = future_branches.iter().filter_map(|&bb| bb).collect();

                // 可能要去到的分支的地址值
                let future_branches_address = successors
                    .iter()
                    .filter_map(|next_basic_block| unsafe { next_basic_block.get_address() })
                    .map(|addr| addr.as_value_ref())
                    .collect::<Vec<_>>();

                if future_branches_address.is_empty() {
                    warn!("(indirect-branch) branch to Meow? future_branches_address.len() < 1!");
                    continue;
                }

                // 如果是条件跳转或者是没有被收集的基本块（why？），构建局部跳转表
                let indirect_branch_table =
                    if br_inst.is_conditional() || !non_entry_bb_addrs.contains(&future_branches_address[0]) {
                        let basic_block_array_ty = ptr_type.array_type(future_branches_address.len() as u32);
                        let array_values = future_branches_address
                            .iter()
                            .map(|v| unsafe { ArrayValue::new(*v) })
                            .collect::<Vec<_>>();

                        let initializer = basic_block_array_ty.const_array(&array_values);
                        let local_indirect_branch_table =
                            module.add_global(basic_block_array_ty, None, ".amice.indirect_branch");
                        local_indirect_branch_table.set_initializer(&initializer);
                        local_indirect_branch_table.set_linkage(Linkage::Private);
                        local_indirect_branch_table.set_constant(true);

                        module.append_to_compiler_used(local_indirect_branch_table);

                        Some(local_indirect_branch_table)
                    } else {
                        // 选择全局跳转表
                        module.get_global(INDIRECT_BRANCH_TABLE_NAME)
                    };

                let Some(indirect_branch_table) = indirect_branch_table else {
                    warn!("(indirect-branch) indirect branch table is None?");
                    continue;
                };

                let builder = ctx.create_builder();
                // 如果是 DummyBlock，则创建一个空的基本块作为目标,
                // 先跳进链式混淆块最后再进真正的块执行代码
                let goal_dummy_block = if self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    let block = ctx.append_basic_block(function, "");
                    builder.position_at_end(block);
                    Some(block)
                } else {
                    builder.position_before(&br_inst);
                    None
                };
                // 获取一下下标，如果是条件跳转，就把i8扩展成i32就好了
                let index = if br_inst.is_conditional() {
                    let cond = br_inst.get_operand(0).unwrap().left().unwrap().into_int_value();
                    builder
                        .build_int_z_extend(cond, i32_type, "")
                        .map_err(|e| warn!("(indirect-branch) build_int_z_extend failed: {e}"))
                        .ok()
                } else {
                    let index = non_entry_bb_addrs.iter().position(|&x| x == future_branches_address[0]);
                    let Some(mut index) = index else {
                        warn!("(indirect-branch) index is None, skipping this branch, branch: {br_inst:?}");
                        continue;
                    };

                    // 加密下标
                    if let Some(xor_key_table) = encrypt_key_global.as_ref() {
                        // if log_enabled!(Level::Debug) {
                        //     debug!("(indirect-branch) encrypt block index: {}", index);
                        // }
                        let xor_key = self.xor_key.as_ref().unwrap();
                        let key_index = index % xor_key.len();
                        index ^= xor_key[key_index] as usize;
                        let enc_index = i32_type.const_int(index as u64, false);
                        let key_gep = builder
                            .build_in_bounds_gep2(
                                xor_key_table.get_value_type().into_array_type(),
                                xor_key_table.as_pointer_value(),
                                &[const_zero, i32_type.const_int(key_index as u64, false)],
                                "",
                            )
                            .map_err(|e| error!("(indirect-branch) build gep_index failed: {e}"))
                            .expect("build gep_index failed");

                        let key_val = builder
                            .build_load2(i32_type, key_gep, "IndirectBranchingKey")
                            .map_err(|e| error!("(indirect-branch) build load failed: {e}"))
                            .expect("build load failed")
                            .into_int_value();

                        builder
                            .build_xor(enc_index, key_val, "IndirectBranchingKey")
                            .map_err(|e| error!("(indirect-branch) build xor failed: {e}"))
                            .expect("build xor failed")
                    } else {
                        i32_type.const_int(index as u64, false)
                    }
                    .into()
                };
                let Some(index) = index else {
                    warn!("(indirect-branch) index is None, skipping this branch, branch: {br_inst:?}");
                    continue;
                };
                let Ok(gep) = builder
                    .build_in_bounds_gep2(
                        indirect_branch_table.get_value_type().into_array_type(),
                        indirect_branch_table.as_pointer_value(),
                        &[const_zero, index],
                        "",
                    )
                    .map_err(|e| error!("(indirect-branch) build gep_index failed: {e}"))
                else {
                    panic!("(indirect-branch) build gep_index failed, this should never happen");
                };
                let Ok(loaded_address) = builder
                    .build_load2(ptr_type, gep, "IndirectBranchingTargetAddress")
                    .map_err(|e| error!("(indirect-branch) build load failed: {e}"))
                else {
                    panic!("(indirect-branch) build load failed, this should never happen");
                };
                let Ok(mut indir_br) = builder
                    .build_indirect_branch(loaded_address.as_basic_value_enum(), &successors)
                    .map_err(|e| error!("(indirect-branch) build indirect branch failed: {e}"))
                else {
                    panic!("(indirect-branch) build indirect branch failed, this should never happen");
                };

                if self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    let max_chain_num = if self.flags.contains(IndirectBranchFlags::ChainedDummyBlock) {
                        13
                    } else {
                        1
                    };
                    let chain_nums = std::cmp::max(1, rand::random_range(0..=max_chain_num));
                    // 目标块
                    let goal_dummy_block = goal_dummy_block.unwrap();

                    let mut cur_dummy_block = goal_dummy_block;
                    for _ in 0..chain_nums - 1 {
                        let dummy_block = ctx.append_basic_block(function, "dummy_block");
                        builder.position_at_end(dummy_block);
                        let target = unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };

                        if self.flags.contains(IndirectBranchFlags::DummyJunk) && rand::random_range(0..=100) < 45 {
                            emit_dummy_junk(&builder, i32_type);
                        }

                        builder
                            .build_indirect_branch(target, &[cur_dummy_block])
                            .map_err(|e| error!("(indirect-branch) build indirect branch failed: {e}"))
                            .expect("build indirect branch failed");
                        cur_dummy_block = dummy_block;
                    }

                    for &target_block in &successors {
                        if let Some(pb) = br_inst.get_parent() {
                            target_block.fix_phi_node(
                                pb,               // 原始前驱块
                                goal_dummy_block, // 新前驱块
                            );
                        } else {
                            warn!("(indirect-branch) branch: {br_inst:?}, parent is None");
                        }
                    }

                    builder.position_before(&br_inst);
                    let target = unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };
                    indir_br = builder
                        .build_indirect_branch(target, &[cur_dummy_block])
                        .map_err(|e| error!("(indirect-branch) build_indirect_branch failed: {e}"))
                        .expect("build_indirect_branch failed");
                }

                // if let Some(old_pred) = br_inst.get_parent()
                //     && let Some(new_pred) = indir_br.get_parent() {
                //     debug!("(indirect-branch) old_pred: {old_pred:?}, new_pred: {new_pred:?}");
                // }

                br_inst.erase_from_basic_block();
            }

            if function.verify_function_bool() {
                warn!("(indirect-branch) function {:?} verify failed", function.get_name());
            }
        }

        PreservedAnalyses::None
    }
}

fn emit_dummy_junk<'ctx>(builder: &Builder<'ctx>, i32_ty: IntType<'ctx>) {
    let dummy_val1 = i32_ty.const_int(rand::random::<u32>() as u64, false);
    let dummy_val2 = i32_ty.const_int(rand::random::<u32>() as u64, false);
    if let Ok(alloca) = builder.build_alloca(i32_ty, "junk_volatile") {
        let _junk_add = builder.build_int_add(dummy_val1, dummy_val2, "junk");
        if rand::random::<bool>() {
            let _ = builder.build_int_compare(IntPredicate::EQ, dummy_val1, dummy_val2, "junk_cmp");
        }
        if rand::random_range(0..=100) < 30 {
            if let Ok(junk_cmp) = builder.build_int_compare(IntPredicate::NE, dummy_val1, dummy_val2, "junk_cmp") {
                let _ = builder
                    .build_int_z_extend(junk_cmp, i32_ty, "junk_cmp_zext")
                    .map(|dummy_val| builder.build_store(alloca, dummy_val))
                    .map(|result| result.map(|store_inst| store_inst.set_volatile(true)));
            }
        }
    }
}

/// 收集所有方法的所有基本块
fn collect_basic_block<'a>(module: &Module<'a>) -> Vec<BasicBlock<'a>> {
    let mut basic_blocks = Vec::new();
    for fun in module.get_functions() {
        let Some(entry_block) = fun.get_entry_block() else {
            continue;
        };
        for bb in fun.get_basic_blocks() {
            if bb == entry_block {
                continue;
            }
            basic_blocks.push(bb);
        }
    }

    basic_blocks
}

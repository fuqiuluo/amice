use crate::llvm_utils::branch_inst::get_successor;
use crate::llvm_utils::function::get_basic_block_entry_ref;
use crate::ptr_type;
use bitflags::bitflags;
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::types::AsTypeRef;
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, BasicValue, InstructionOpcode, PhiValue};
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use llvm_plugin::inkwell::context::{Context, ContextRef};
use llvm_plugin::inkwell::llvm_sys::core::LLVMAddIncoming;
use llvm_plugin::inkwell::llvm_sys::prelude::{LLVMBasicBlockRef, LLVMValueRef};
use log::{debug, error, log_enabled, warn, Level};
use crate::utils::config_utils::{CONFIG, IndirectBranchFlags};
use rand::Rng;

const INDIRECT_BRANCH_TABLE_NAME: &str = "global_indirect_branch_table";

pub struct IndirectBranch {
    enable: bool,
    flags: IndirectBranchFlags,
    xor_key: Option<[u32; 4]>,
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

        let encrypt_key_table = if self.flags.contains(IndirectBranchFlags::EncryptBlockIndex) {
            let xor_key = self
                .xor_key
                .as_ref()
                .unwrap()
                .iter()
                .map(|x| i32_ty.const_int(*x as u64, false))
                .map(|addr| addr.as_value_ref())
                .collect::<Vec<_>>();
            let xor_key_array_ty = i32_ty.array_type(xor_key.len() as u32);
            let initializer = unsafe {
                ArrayValue::new_raw_const_array(xor_key_array_ty.as_type_ref(), &xor_key)
            };
            let table = module.add_global(xor_key_array_ty, None, ".amice.indirect_branch_key");
            table.set_initializer(&initializer);
            table.set_linkage(Linkage::Private);
            table.set_constant(false);
            Some(table)
        } else {
            None
        };

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
                    // future_branches的[1]是内存下标，get_successor(0)为真块
                    // 当为真时，把bool扩展为i32,则这个i32的值是1，直接作为下标使用即future_branches[1]应该保存真分支
                    future_branches[1] = get_successor(bi, 0).unwrap().right();
                    future_branches[0] = get_successor(bi, 1).unwrap().right();
                } else {
                    future_branches[0] = get_successor(bi, 0)
                        //.ok_or(anyhow!("block: {}, ops = {:?}", bi, bi.get_operands().collect::<Vec<_>>()))
                        .expect("no successor for basic block")
                        .right(); // true分支
                }

                // 可能要去到的分支
                let future_branches: Vec<_> = future_branches.iter().filter_map(|&bb| bb).collect();

                // 可能要去到的分支的地址值
                let future_branches_address = future_branches
                    .iter()
                    .filter_map(|next_basic_block| unsafe { next_basic_block.get_address() })
                    .map(|addr| addr.as_value_ref())
                    .collect::<Vec<_>>();

                if future_branches_address.is_empty() {
                    warn!("(indirect-branch) branch to Meow? future_branches_address.len() < 1!");
                    continue;
                }

                // 如果是条件跳转或者是没有被收集的基本块（why？），构建局部跳转表
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
                    let block = ctx.append_basic_block(fun, "");
                    builder.position_at_end(block);
                    Some(block)
                } else {
                    builder.position_before(&bi);
                    None
                };
                // 获取一下下标，如果是条件跳转，就把i8扩展成i32就好了
                let index = if bi.is_conditional() {
                    let cond = bi.get_operand(0).unwrap().left().unwrap()
                        .into_int_value();
                    builder
                        .build_int_z_extend(cond, i32_ty, "")
                        .map_err(|e| warn!("(indirect-branch) build_int_z_extend failed: {e}"))
                        .ok()
                } else {
                    let index = basic_block_array
                        .iter()
                        .position(|&x| x == future_branches_address[0]);
                    let Some(mut index) = index else {
                        warn!(
                            "(indirect-branch) index is None, skipping this branch, branch: {bi:?}"
                        );
                        continue;
                    };

                    // 加密下标
                    if let Some(xor_key_table) = encrypt_key_table.as_ref() {
                        if log_enabled!(Level::Debug) {
                            debug!("(indirect-branch) encrypt block index: {}", index);
                        }
                        let xor_key = self.xor_key.as_ref().unwrap();
                        let key_index = index % xor_key.len();
                        index ^= xor_key[key_index] as usize;
                        let enc_index = i32_ty.const_int(index as u64, false);
                        let key_gep = unsafe {
                            builder.build_in_bounds_gep(
                                xor_key_table.get_value_type().into_array_type(),
                                xor_key_table.as_pointer_value(),
                                &[const_zero, i32_ty.const_int(key_index as u64, false)],
                                "",
                            )
                        }
                        .map_err(|e| error!("(indirect-branch) build gep_index failed: {e}"))
                        .expect("build gep_index failed");
                        let key_val = builder
                            .build_load(i32_ty, key_gep, "IndirectBranchingKey")
                            .map_err(|e| error!("(indirect-branch) build load failed: {e}"))
                            .expect("build load failed")
                            .into_int_value();
                        

                        builder
                            .build_xor(enc_index, key_val, "IndirectBranchingKey")
                            .map_err(|e| error!("(indirect-branch) build xor failed: {e}"))
                            .expect("build xor failed")
                    } else {
                        i32_ty.const_int(index as u64, false)
                    }
                    .into()
                };
                let Some(index) = index else {
                    warn!("(indirect-branch) index is None, skipping this branch, branch: {bi:?}");
                    continue;
                };
                let Ok(gep) = (unsafe {
                    builder
                        .build_in_bounds_gep(
                            indirect_branch_table.get_value_type().into_array_type(),
                            indirect_branch_table.as_pointer_value(),
                            &[const_zero, index],
                            "",
                        )
                        .map_err(|e| error!("(indirect-branch) build gep_index failed: {e}"))
                }) else {
                    panic!("(indirect-branch) build gep_index failed, this should never happen");
                };
                let Ok(loaded_address) = builder
                    .build_load(ptr_ty, gep, "IndirectBranchingTargetAddress")
                    .map_err(|e| error!("(indirect-branch) build load failed: {e}"))
                else {
                    panic!("(indirect-branch) build load failed, this should never happen");
                };
                let Ok(mut indir_br) = builder
                    .build_indirect_branch(loaded_address.as_basic_value_enum(), &future_branches)
                    .map_err(|e| error!("(indirect-branch) build indirect branch failed: {e}"))
                else {
                    panic!(
                        "(indirect-branch) build indirect branch failed, this should never happen"
                    );
                };

                if self.flags.contains(IndirectBranchFlags::DummyBlock) {
                    let max_chain_num = if self.flags.contains(IndirectBranchFlags::ChainedDummyBlock) { 13 } else { 1 };
                    let chain_nums = std::cmp::max(1, rand::random_range(0..=max_chain_num));
                    // 目标块
                    let goal_dummy_block = goal_dummy_block.unwrap();

                    let mut cur_dummy_block = goal_dummy_block;
                    for _ in 0..chain_nums - 1 {
                        let dummy_block = ctx.append_basic_block(fun, "");
                        builder.position_at_end(dummy_block);
                        let target =
                            unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };

                        /*                        if rand::random_range(0..=100) < 45 {
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
                        }*/

                        builder
                            .build_indirect_branch(target, &[cur_dummy_block])
                            .map_err(|e| {
                                error!("(indirect-branch) build indirect branch failed: {e}")
                            })
                            .expect("build indirect branch failed");
                        cur_dummy_block = dummy_block;
                    }

                    for &target_block in &future_branches {
                        if let Some(pb) = bi.get_parent() {
                            update_phi_nodes(
                                ctx,
                                pb, // 原始前驱块
                                goal_dummy_block, // 新前驱块
                                target_block,     // 目标块
                            );
                        } else {
                            warn!("(indirect-branch) branch: {bi:?}, parent is None");
                        }
                    }

                    builder.position_before(&bi);
                    let target =
                        unsafe { cur_dummy_block.get_address().unwrap().as_basic_value_enum() };
                    indir_br = builder
                        .build_indirect_branch(target, &[cur_dummy_block])
                        .map_err(|e| error!("(indirect-branch) build_indirect_branch failed: {e}"))
                        .expect("build_indirect_branch failed");
                }

                bi.replace_all_uses_with(&indir_br);
                bi.remove_from_basic_block();
            }

            unsafe {
                if amice_llvm::ir::function::verify_function(
                    fun.as_value_ref() as *mut std::ffi::c_void
                ) {
                    warn!(
                        "(indirect-branch) function {} verify failed",
                        fun.get_name().to_str().unwrap_or("<unknown>")
                    );
                }
            }

            //unsafe { amice_llvm::ir::function::fix_stack(fun.as_value_ref() as *mut std::ffi::c_void) }
        }

        PreservedAnalyses::None
    }
}

fn update_phi_nodes<'ctx>(
    ctx: ContextRef,
    old_pred: BasicBlock<'ctx>,
    new_pred: BasicBlock<'ctx>,
    target_block: BasicBlock<'ctx>,
) {
    let builder = ctx.create_builder();

    for phi in target_block.get_first_instruction().iter() {
        if phi.get_opcode() != InstructionOpcode::Phi {
            break;
        }

        // %25 = phi i32 [ 1, %21 ], [ %23, %22 ]
        builder.position_before(phi);
        let phi = unsafe { PhiValue::new(phi.as_value_ref()) };
        let incoming_vec = phi.get_incomings()
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

/// 收集所有方法的所有基本块
fn collect_basic_block<'a>(module: &Module<'a>) -> Vec<BasicBlock<'a>> {
    let mut basic_blocks = Vec::new();
    for fun in module.get_functions() {
        let entry_block = get_basic_block_entry_ref(&fun);
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
        flags |= CONFIG.indirect_branch.flags;

        if enable {
            debug!("IndirectBranch pass enabled with flags: {flags:?}");
        }

        let xor_key = if flags.contains(IndirectBranchFlags::EncryptBlockIndex) {
            let mut xor_key = [0u32; 4];
            rand::rng().fill(&mut xor_key[..]);
            xor_key.into()
        } else {
            None
        };

        Self {
            enable,
            flags,
            xor_key,
        }
    }
}

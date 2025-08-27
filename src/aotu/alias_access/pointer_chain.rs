use crate::aotu::alias_access::{AliasAccess, AliasAccessAlgo};
use amice_llvm::inkwell2::AdvancedInkwellBuilder;
use amice_llvm::ir::basic_block::get_first_insertion_pt;
use amice_llvm::ir::function::get_basic_block_entry;
use amice_llvm::ptr_type;
use anyhow::anyhow;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::types::{BasicType, StructType};
use llvm_plugin::inkwell::values::{AnyValue, AsValueRef, BasicValue, FunctionValue, InstructionOpcode, PointerValue};
use log::{debug, warn};
use std::collections::HashMap;

const META_BOX_COUNT: usize = 6;

struct ElementPos<'ctx> {
    st: StructType<'ctx>,
    index: u32,
}

struct ReferenceNode<'ctx> {
    is_raw: bool,
    id: u32,
    /// Raw: RawBox*; 非Raw: MetaBox*
    /// 保存BaseBox的地方，可以是两种盒子的任意一种
    alloca: PointerValue<'ctx>,
    /// 仅 Raw 用：原 Alloca -> 在该 RawBox 中的位置
    /// 这里两个map的key是原始的alloca的数据地址
    /// 如果你在raw_insts发现了你的原始alloca数据地址，说明这里的alloca是指向RawBox的
    raw_insts: Option<HashMap<PointerValue<'ctx>, ElementPos<'ctx>>>,
    /// 仅 非Raw 用：原 Alloca -> 在本层可走的 idx 列表
    path: Option<HashMap<PointerValue<'ctx>, Vec<usize>>>,
    /// 仅 非Raw 用：idx -> 子节点
    /// 指向 Graph 中的下标
    edges: Option<HashMap<usize, usize>>,
}

fn get_random_no_repeat(n: usize, k: usize) -> Vec<usize> {
    let mut v = (0..n).collect::<Vec<_>>();
    // 洗牌后取前 k 个
    for i in (1..v.len()).rev() {
        let j = rand::random_range(0..=i);
        v.swap(i, j);
    }
    v.truncate(k);
    v
}

fn build_getter_function<'ctx>(
    module: &Module<'ctx>,
    st: StructType<'ctx>,
    idx: u32,
) -> anyhow::Result<FunctionValue<'ctx>> {
    let ctx = module.get_context();
    let ptr_ty = ptr_type!(ctx, i8_type);
    let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);

    let func = module.add_function("", fn_ty, None);
    let entry = ctx.append_basic_block(func, "entry");
    let builder = ctx.create_builder();

    builder.position_at_end(entry);

    let arg0 = func.get_nth_param(0).unwrap().into_pointer_value();

    // cast void* -> MetaBox*
    let trans_ptr = builder.build_pointer_cast(arg0, st.ptr_type(AddressSpace::default()), "cast_trans")?;

    // &p->slot[idx]
    let slot_addr = builder
        .build_struct_gep2(st, trans_ptr, idx, "slot_addr")
        .expect("GEP slot");

    // 为隐藏类型信息，返回 void*
    let ret = builder.build_pointer_cast(slot_addr, ptr_ty, "as_ptr")?;
    builder.build_return(Some(&ret))?;

    Ok(func)
}

#[derive(Default)]
pub(super) struct PointerChainAlgo;

impl AliasAccessAlgo for PointerChainAlgo {
    fn initialize(&mut self, _pass: &AliasAccess) -> anyhow::Result<()> {
        Ok(())
    }

    fn do_alias_access(&mut self, pass: &AliasAccess, module: &Module<'_>) -> anyhow::Result<()> {
        for function in module.get_functions() {
            do_alias_access_pointer_chain(pass, module, function)?;
        }

        Ok(())
    }
}

fn do_alias_access_pointer_chain(
    pass: &AliasAccess,
    module: &Module<'_>,
    function: FunctionValue,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i8_ty = ctx.i8_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ty = ctx.i32_type();
    let ptr_ty = ptr_type!(ctx, i8_type);

    let mut allocas = Vec::new();
    for bb in function.get_basic_blocks() {
        for instr in bb.get_instructions() {
            if instr.get_opcode() == InstructionOpcode::Alloca {
                let any_value_enum = instr.as_any_value_enum();
                if !any_value_enum.is_pointer_value() {
                    return Err(anyhow::anyhow!("Alloca must be pointer type: {:?}", any_value_enum));
                }
                let alignment = instr
                    .get_alignment()
                    .map_err(|e| anyhow!("failed to get alloca alignment: {}", e))?;
                if alignment <= 8 {
                    allocas.push(any_value_enum.into_pointer_value());
                }
            }
        }
    }
    if allocas.is_empty() {
        return Ok(());
    }

    //debug!("(alias-access) function {:?} has {:?} allocas", function.get_name(), allocas.len());

    // 分桶，把数据随机放进各个RawBox里面，至于是哪个？纯随机！
    let mut buckets = vec![Vec::new(); allocas.len()];
    for &ai in &allocas {
        let idx = rand::random_range(0..allocas.len());
        buckets[idx].push(ai);
    }

    let Some(entry_block) = get_basic_block_entry(function) else {
        return Err(anyhow::anyhow!("function {:?} has no entry block", function.get_name()));
    };
    let first_insertion_pt = get_first_insertion_pt(entry_block);

    let builder = ctx.create_builder();
    builder.position_before(&first_insertion_pt);

    // 这里alloc出来的有两种类型：
    // 第一种是真正装着数据的盒子，我们管他叫RawBox
    // 第二种是装着RawBox的盒子，我们管他叫MetaBox
    // 这两个box的共同点是有很多个格子，我们在要格子里面放Box，可以是RawBox也可以是MetaBox
    // 当然这得是随机的，这样就构成了random pointer chain
    let mut graph: Vec<ReferenceNode> = Vec::new();
    let mut node_id: u32 = 0;

    for items in buckets.into_iter().filter(|b| !b.is_empty()) {
        let mut items = items.into_iter().map(|p| (false, p.into())).collect::<Vec<_>>();

        if pass.loose_raw_box {
            // 填充幽灵数据
            for i in 0..rand::random_range(0..items.len() + 1) {
                let j = rand::random_range(0..=i);
                items.insert(j, (true, None));
            }
        };

        if pass.shuffle_raw_box {
            for i in (1..items.len()).rev() {
                let j = rand::random_range(0..=i);
                items.swap(i, j);
            }
        }

        let mut allocated_types = Vec::new();
        for (is_phantom, ptr) in items.iter() {
            if !is_phantom && let Some(ptr) = ptr {
                let typ = ptr
                    .as_instruction_value()
                    .ok_or_else(|| anyhow!("failed to get alloca instruction value"))?
                    .get_allocated_type()
                    .map_err(|e| anyhow!("failed to get alloca allocated type: {}", e))?;
                allocated_types.push(typ);
            } else {
                allocated_types.push(i8_ptr.as_basic_type_enum());
            }
        }

        assert_eq!(allocated_types.len(), items.len());

        let st = ctx.opaque_struct_type("amice.alias.st");
        st.set_body(&allocated_types, false);

        // 分配 Raw 盒子
        let raw_alloca = builder.build_alloca(st, "raw_box")?;

        // 记录 Slot 映射：每个原 alloca -> (st, index)
        // 不记录幽灵数据！
        let mut raw_map: HashMap<PointerValue, ElementPos> = HashMap::new();
        for (idx, &ai) in items.iter().enumerate() {
            let (is_phantom, ai) = ai;
            if !is_phantom && let Some(ai) = ai {
                raw_map.insert(ai, ElementPos { st, index: idx as u32 });
            }
        }

        graph.push(ReferenceNode {
            is_raw: true,
            id: {
                let x = node_id;
                node_id += 1;
                x
            },
            alloca: raw_alloca,
            raw_insts: raw_map.into(),
            path: None,
            edges: None,
        });
    }

    if graph.is_empty() {
        warn!("(alias-access) no allocas found");
        return Ok(());
    }

    let meta_box_count = graph.len().saturating_mul(3).max(1);
    debug!("(alias-access) meta box count: {}", meta_box_count);

    // 构造元盒类型
    let meta_box_ty = ctx.struct_type(&vec![i8_ptr.as_basic_type_enum(); META_BOX_COUNT], false);

    for _ in 0..meta_box_count {
        let meta_box_alloca = builder.build_alloca(meta_box_ty, "")?;
        // 随机选择使用多少分支
        let bn = rand::random_range(0..META_BOX_COUNT);
        let idxs = get_random_no_repeat(META_BOX_COUNT, bn);

        let mut edges = HashMap::new();
        let mut path = HashMap::new();
        for idx in idxs {
            let child_id = rand::random_range(0..graph.len());
            let child = &graph[child_id];

            let slot_addr = builder
                .build_struct_gep2(meta_box_ty, meta_box_alloca, idx as u32, "slot")
                .expect("router gep");

            builder.build_store(slot_addr, child.alloca)?;

            if child.is_raw {
                for (ai, _) in child.raw_insts.as_ref().unwrap() {
                    path.entry(*ai).or_insert_with(Vec::new).push(idx);
                }
            } else {
                for (ai, _) in child.path.as_ref().unwrap() {
                    path.entry(*ai).or_insert_with(Vec::new).push(idx);
                }
            }
            edges.insert(idx, child_id);

            //debug!("idx {} -> child {}", idx, child_id);
        }

        let rn = ReferenceNode {
            is_raw: false,
            id: {
                let x = node_id;
                node_id += 1;
                x
            },
            alloca: meta_box_alloca,
            raw_insts: None,
            path: path.into(),
            edges: edges.into(),
        };
        graph.push(rn);
    }

    // 为每个 idx 缓存/构造 getter 函数
    let mut getters: HashMap<u32, LLVMValueRef> = HashMap::new();
    let get_getter = |idx: u32, getters: &mut HashMap<u32, LLVMValueRef>| -> anyhow::Result<FunctionValue> {
        if let Some(fv) = getters.get(&idx) {
            let g = unsafe { FunctionValue::new(*fv) }.ok_or(anyhow!("failed to get existing getter function"))?;
            Ok(g)
        } else {
            let g = build_getter_function(module, meta_box_ty, idx)?;
            getters.insert(idx, g.as_value_ref());
            Ok(g)
        }
    };

    for bb in function.get_basic_blocks() {
        for instr in bb.get_instructions() {
            for i in 0..instr.get_num_operands() {
                if let Some(opnd) = instr.get_operand(i)
                    && let Some(op_left) = opnd.left()
                    && op_left.is_pointer_value()
                {
                    let op_ptr = op_left.into_pointer_value();
                    // 是否是参与混淆的原 Alloca
                    if !allocas.iter().any(|&a| a == op_ptr) {
                        continue;
                    }

                    // 找一个能到达该 Alloca 的入口节点
                    let mut entry_node_idx = None;
                    // 打乱顺序
                    let mut order: Vec<usize> = (0..graph.len()).collect();
                    for j in (1..order.len()).rev() {
                        let k = rand::random_range(0..=j);
                        order.swap(j, k);
                    }
                    for gi in order {
                        let node = &graph[gi];
                        let hit = if node.is_raw {
                            node.raw_insts
                                .as_ref()
                                .ok_or(anyhow!("raw_insts is None"))?
                                .contains_key(&op_ptr)
                        } else {
                            node.path.as_ref().ok_or(anyhow!("path is None"))?.contains_key(&op_ptr)
                        };
                        if hit {
                            entry_node_idx = Some(gi);
                            break;
                        }
                    }
                    let mut cur_idx = entry_node_idx.expect("reachable node");
                    let mut cur_ptr = graph[cur_idx].alloca; // VP

                    // 逐层下钻
                    while !graph[cur_idx].is_raw {
                        // MetaBox里面有至少有一个slot保存了直接指向或者间接指向RawBox的指针
                        // 这里的idxs就是这些slot的下标
                        let idxs = &graph[cur_idx].path.as_ref().ok_or(anyhow!("path is None"))?[&op_ptr];
                        // 随机选一个slot，可能路径都是可行的
                        let pick = idxs[rand::random_range(0..idxs.len())];

                        // 获取一个getter，这个getter就是从目标位置的index=pick的位置，把那个结构体的字段拿出来
                        let gfunc = get_getter(pick as u32, &mut getters)?;

                        // 调 getter(void*) -> void*
                        // 先把当前指针 cast 到 void*
                        let cast_in = builder.build_pointer_cast(cur_ptr, ptr_ty, "as_ptr")?;
                        let call = builder.build_call(gfunc, &[cast_in.into()], "callg")?;
                        // 把返回的 void* 再 cast 回 BaseBox**，再 load 出下一层指针（MetaBox* 或 RawBox*）
                        let slot_addr = call.as_any_value_enum().into_pointer_value();
                        let slot_addr_base_box_ptr = builder.build_pointer_cast(
                            slot_addr,
                            ptr_ty.ptr_type(AddressSpace::default()),
                            "as_ptr_ptr",
                        )?;
                        let next_ptr = builder
                            .build_load2(ptr_ty, slot_addr_base_box_ptr, "ld_next")?
                            .into_pointer_value();

                        // child pointer 还是 void*；把它转回“通用指针类型”以继续链（这里我们统一用 void*，直到最后再按需 cast）
                        cur_ptr = next_ptr;
                        // 跟踪图下标
                        cur_idx = graph[cur_idx].edges.as_ref().ok_or(anyhow!("edges is None"))?[&pick];
                    }

                    // 到达 RawBox：GEP 到字段
                    let ep = &graph[cur_idx].raw_insts.as_ref().ok_or(anyhow!("raw_insts is None"))?[&op_ptr];

                    // 当前 cur_ptr 是 RawBox* 的“真实类型”吗？我们一路保持 void*，需要 cast 回 RawBox*
                    let st_ptr_ty = ep.st.ptr_type(AddressSpace::default());
                    let raw_ptr = builder.build_pointer_cast(cur_ptr, st_ptr_ty, "as_raw_st")?;

                    let field_addr = builder
                        .build_struct_gep2(ep.st, raw_ptr, ep.index, "field")
                        .expect("field gep");

                    // 用这个地址替换当前指令的第 i 个操作数
                    instr.set_operand(i, field_addr.as_basic_value_enum());
                }
            }
        }
    }

    for &ai in &allocas {
        let inst = ai.as_instruction_value().expect("alloca inst");
        inst.erase_from_basic_block();
    }

    Ok(())
}

use crate::aotu::string_encryption::{
    EncryptedGlobalValue, STACK_ALLOC_THRESHOLD, StringEncryption, StringEncryptionAlgo, alloc_stack_string,
    array_as_const_string, collect_insert_points,
};
use crate::config::{StringDecryptTiming as DecryptTiming, StringEncryptionConfig};
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, LLVMValueRefExt, ModuleExt};
use amice_llvm::ptr_type;
use anyhow::anyhow;
use llvm_plugin::inkwell;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{
    ArrayValue, AsValueRef, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, InstructionOpcode,
};
use log::{Level, debug, error, log_enabled, warn};
use rand::Rng;

#[derive(Default)]
pub(super) struct SimdXorAlgo {
    pub(super) key: [u8; 32],
}

impl StringEncryptionAlgo for SimdXorAlgo {
    fn initialize(&mut self, _cfg: &StringEncryptionConfig, _module: &mut Module<'_>) -> anyhow::Result<()> {
        rand::rng().fill(&mut self.key);
        Ok(())
    }

    fn do_string_encrypt(&mut self, cfg: &StringEncryptionConfig, module: &mut Module<'_>) -> anyhow::Result<()> {
        do_handle(cfg, module, &self.key)
    }
}

fn do_handle<'a>(cfg: &StringEncryptionConfig, module: &mut Module<'a>, key: &[u8; 32]) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i8_ty = ctx.i8_type();
    let vector256 = i8_ty.array_type(32);

    let is_lazy_mode = matches!(cfg.timing, DecryptTiming::Lazy);
    let is_global_mode = matches!(cfg.timing, DecryptTiming::Global);

    let global_key = module.add_global(vector256, Some(AddressSpace::default()), "");
    let array_values = key
        .map(|c| i8_ty.const_int(c as u64, false))
        .map(|v| unsafe { ArrayValue::new(v.as_value_ref()) });
    global_key.set_initializer(&vector256.const_array(&array_values));

    let string_global_values: Vec<EncryptedGlobalValue<'a>> = module
        .get_globals()
        .filter(|global| !matches!(global.get_linkage(), Linkage::External))
        .filter(|global| {
            (!cfg.only_dot_str || global.get_name().to_str().is_ok_and(|s| s.contains(".str")))
                && global
                    .get_section()
                    .is_none_or(|section| section.to_str() != Ok("llvm.metadata"))
        })
        .filter_map(|global| {
            let init = global.get_initializer()?;
            let mut string_fields = Vec::new();
            let mut struct_value: Option<_> = None;

            match init {
                // 情况 A: 传统的 C-style 字符串直接就是 ArrayValue
                BasicValueEnum::ArrayValue(arr) => {
                    if arr.is_const_string() && arr.get_type().len() > 1 {
                        string_fields.push((None, arr)); // None 表示不是结构体成员
                    }
                },
                // 情况 B: 结构体（适配 Rust String, C++ NTTP 结构体等）
                BasicValueEnum::StructValue(stru) => {
                    for i in 0..stru.count_fields() {
                        if let Some(field) = stru.get_field_at_index(i) {
                            if let BasicValueEnum::ArrayValue(arr) = field {
                                if arr.is_const_string() && arr.get_type().len() > 1 {
                                    string_fields.push((Some(i), arr)); // 记录字段索引
                                }
                            }
                        }
                    }
                    struct_value = Some(stru);
                },
                _ => return None,
            }

            if string_fields.is_empty() {
                None
            } else {
                // 返回全局变量及其内部所有的待加密字符串信息
                Some((global, struct_value, string_fields))
            }
        })
        .flat_map(|(global, stru, string_fields)| {
            // 这里的 string_fields 是 Vec<(Option<usize>, ArrayValue)>
            string_fields.into_iter().filter_map(move |(field_idx, arr)| {
                if !arr.is_const_string() {
                    debug!(
                        "(strenc) skipping field in {:?}: is_const_string() false",
                        global.get_name()
                    );
                    return None;
                }

                let ty = arr.get_type();
                if ty.is_empty() || ty.len() <= 1 {
                    debug!(
                        "(strenc) skipping field in {:?}: too short (len={})",
                        global.get_name(),
                        ty.len()
                    );
                    return None;
                }

                // 校验通过，返回一个包含上下文的元组，方便后面加密逻辑使用
                // 这里的 stru 现在需要重新考虑，因为一个 global 可能对应多个字段
                Some((global, stru, field_idx, arr))
            })
        })
        .inspect(|(a, b, c, d)| {
            // do nothing!
        })
        .filter_map(|(global, stru, field_idx, arr)| {
            let s = array_as_const_string(&arr).and_then(|s| str::from_utf8(s).ok())?;
            let mut encoded_str = vec![0u8; s.len()];
            for (i, c) in s.bytes().enumerate() {
                encoded_str[i] = c ^ key[i % key.len()];
            }

            let unique_name = global.get_name().to_str().map_or_else(
                |_| format!("ref_{:x}", global.as_value_ref() as usize),
                |s| s.to_string(),
            );
            Some((unique_name, global, stru, field_idx, encoded_str))
        })
        .map(|(unique_name, global, stru, field_idx, encoded_str)| {
            let string_len = encoded_str.len() as u32;
            let mut should_use_stack = cfg.stack_alloc && string_len <= STACK_ALLOC_THRESHOLD;

            // Warn if stack allocation is requested but string is too large
            if cfg.stack_alloc && string_len > STACK_ALLOC_THRESHOLD {
                warn!(
                    "(strenc) string '{}' ({}B) exceeds 4KB limit for stack allocation, using global timing instead",
                    unique_name, string_len
                );
            }

            let mut users = Vec::new();

            if !is_global_mode && (is_lazy_mode) {
                let mut use_opt = global.get_first_use();
                while let Some(u) = use_opt {
                    use_opt = u.get_next_use();

                    let mut temp_user = Vec::new();
                    if let Err(e) = collect_insert_points(global, u.get_user(), &mut temp_user) {
                        error!("(strenc) failed to collect insert points: {e}");
                    }
                    if temp_user.is_empty() {
                        // 保证非直接引用的解密下降正常运行，需要清空
                        users.clear();
                        should_use_stack = false;
                        break;
                    }

                    // 检查是否有PHI节点引用此字符串
                    // PHI节点的操作数必须在前驱块中定义，无法使用栈分配模式，必须降级到回写模式
                    for (value_ref, _) in &temp_user {
                        let inst = value_ref.into_instruction_value();
                        if inst.get_opcode() == InstructionOpcode::Phi {
                            debug!(
                                "(strenc) string '{}' is referenced by PHI node, disabling stack allocation",
                                unique_name
                            );
                            should_use_stack = false;
                            break;
                        }
                    }

                    users.append(&mut temp_user);
                }

                if users.is_empty() {
                    // 找不到调用点的字符串
                    should_use_stack = false;
                }
            }

            // 当需要回写解密的时候，一个flag是必须的，虽然我没有办法保证线程安全
            // 什么时候是回写解密？是懒加载开启且不是在栈上解密的情况下！
            // 全局函数解密不需要flag，省去这个步骤
            let flag = if is_lazy_mode && !should_use_stack {
                // 必须使用跟 global string 一样的名字后缀，确保不同模块生成的 flag 名字一致
                let flag_name = format!("dec_flag_{}", global.get_name().to_str().unwrap());
                let flag = if let Some(existing) = module.get_global(&flag_name) {
                    existing
                } else {
                    let new_flag = module.add_global(i32_ty, None, &flag_name);
                    new_flag.set_initializer(&i32_ty.const_int(0, false));
                    let str_linkage = global.get_linkage();
                    match str_linkage {
                        // 如果字符串是私有的，Flag 也是私有的
                        Linkage::Internal | Linkage::Private => {
                            new_flag.set_linkage(Linkage::Internal);
                        },
                        // 如果字符串是 LinkOnce/Weak (可合并的)，Flag 也必须是 LinkOnceODR/WeakODR
                        // 这样链接器会把多个模块的 Flag 合并成一个
                        Linkage::LinkOnceODR | Linkage::WeakODR | Linkage::LinkOnceAny | Linkage::WeakAny => {
                            // 使用 LinkOnceODR 确保合并，并且如果没用到的模块可以丢弃
                            // new_flag.set_linkage(Linkage::LinkOnceODR);
                            // LinkOnce 需要设置 comdat 组吗？通常为了保险最好设为 weakODR 或者同组
                            // 简单起见，WeakODR 比较通用，不仅合并而且保证存在
                            new_flag.set_linkage(Linkage::WeakODR);
                        },
                        // 其他情况保守起见设为 WeakODR 或跳过加密
                        _ => {
                            new_flag.set_linkage(Linkage::WeakODR);
                        },
                    }
                    // 只有 LinkOnce/Weak 需要设置 comdat，否则链接器可能会报错
                    // 如果你不想处理复杂的 Comdat，最简单的办法是把 Shared 的 flag 设为 WeakODR
                    new_flag
                };
                Some(flag)
            } else {
                None
            };

            // 判断是不是C like的基于ArrayValue的字符串
            // 如果为false就是Rust/C++ NTTP的string, 这个时候一个结构体里面可能出现多个字符串
            // 需要struct gep回去结构体底下指向字符串的指针，而不是指向结构体的头
            let mut is_array_string = true;
            if let Some(stru) = stru {
                // Rust-like strings or C++ NTTP string
                let new_const = ctx.const_string(&encoded_str, false);
                if let Some(field_idx) = field_idx {
                    is_array_string = false;
                    stru.set_field_at_index(field_idx, new_const);
                } else {
                    stru.set_field_at_index(0, new_const);
                }
                global.set_initializer(&stru);
            } else {
                // C-like strings
                let new_const = ctx.const_string(&encoded_str, false);
                global.set_initializer(&new_const);
            }

            // 如果有flag的话 ===> 回写模式，字符串不能是一个常量
            if !flag.is_none() || is_global_mode {
                global.set_constant(false);
            }

            if is_array_string {
                EncryptedGlobalValue::new_array_string(global, string_len, flag, should_use_stack, users)
            } else {
                EncryptedGlobalValue::new_struct_string(
                    global,
                    string_len,
                    flag,
                    should_use_stack,
                    users,
                    stru,
                    field_idx,
                )
            }
        })
        .collect();

    let decrypt_fn = add_decrypt_function(
        module,
        &format!("simd_xor_cipher_{}", rand::random::<u32>()),
        is_lazy_mode,
        cfg.inline_decrypt,
        cfg.stack_alloc,
    )?;

    match cfg.timing {
        DecryptTiming::Lazy => {
            // 按解密方法分隔字符串
            let mut stack_strings = Vec::new();
            let mut write_back_strings = Vec::new();

            for string_value in &string_global_values {
                if string_value.use_stack_alloc {
                    stack_strings.push(*string_value);
                } else {
                    write_back_strings.push(*string_value);
                    string_value.global.set_constant(false);
                }
            }

            if !stack_strings.is_empty() {
                emit_decrypt_before_inst(
                    module,
                    stack_strings,
                    decrypt_fn,
                    true,
                    cfg.allow_non_entry_stack_alloc,
                    global_key,
                )?;
            }
            if !write_back_strings.is_empty() {
                emit_decrypt_before_inst(module, write_back_strings, decrypt_fn, false, false, global_key)?;
            }
        },
        DecryptTiming::Global => {
            emit_global_string_decryptor_ctor(module, &string_global_values, decrypt_fn, global_key)?
        },
    }

    for x in string_global_values {
        x.free();
    }

    Ok(())
}

fn emit_decrypt_before_inst<'a>(
    module: &mut Module<'a>,
    strings: Vec<EncryptedGlobalValue<'a>>,
    decrypt_fn: FunctionValue<'a>,
    stack_alloc: bool,
    allow_non_entry_stack_alloc: bool,
    global_key: GlobalValue,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i32_ptr = ptr_type!(ctx, i32_type);

    let mut undirectly_use_strings = Vec::new();
    for string in strings {
        assert_eq!(stack_alloc, string.use_stack_alloc);
        if !stack_alloc {
            string.global.set_constant(false);
        }

        let builder = ctx.create_builder();
        let user_slice = string.user_slice();

        if !user_slice.is_empty() {
            for (inst, op_index) in user_slice {
                // 如果指令是PHI节点，需要在基本块的第一个非PHI指令位置插入解密代码
                // PHI节点必须在基本块开头，不能在PHI节点前插入任何非PHI指令
                let insert_point = if inst.get_opcode() == InstructionOpcode::Phi {
                    if let Some(parent_bb) = inst.get_parent() {
                        parent_bb.get_first_insertion_pt()
                    } else {
                        error!("(strenc) PHI instruction has no parent block: {inst:?}");
                        *inst
                    }
                } else {
                    *inst
                };

                builder.position_before(&insert_point);

                let ptr = if string.is_array_string() {
                    // 如果是c风格的基于ArrayValue的字符串，那么全局常量本身就是这个字符串
                    string.global.as_pointer_value()
                } else {
                    assert!(string.is_struct_string());
                    let Some(struct_value) = &string.struct_value else {
                        panic!("string.struct_value must be Some(StructValue)");
                    };
                    let Some(field_idx) = string.field_index else {
                        panic!("string.field_index must be Some(u32)");
                    };
                    let global_ptr = string.global.as_pointer_value();
                    let value_type = string.global.get_value_type().into_struct_type();

                    if log_enabled!(Level::Debug) {
                        debug!(
                            "StructString found, type = {:?}, value = {:?}, idx = {}",
                            value_type, struct_value, field_idx
                        );
                    }

                    // 生成 GEP: getelementptr inbounds %struct..., ptr @global, i32 0, i32 field_idx
                    builder.build_struct_gep2(value_type, global_ptr, field_idx, "field_gep")?
                };
                let len_val = i32_ty.const_int(string.str_len as u64, false);

                if stack_alloc
                    && let Ok(container) = alloc_stack_string(module, string, allow_non_entry_stack_alloc, inst)
                {
                    string.flag.map(|flag| unsafe { flag.delete() }); // 不用当然要删除！
                    let flag_ptr = i32_ptr.const_null();
                    let dst = container;
                    let global_key = global_key.as_pointer_value();
                    builder.build_call(
                        decrypt_fn,
                        &[
                            ptr.into(),
                            dst.into(),
                            len_val.into(),
                            global_key.into(),
                            flag_ptr.into(),
                        ],
                        "",
                    )?;

                    if !inst.set_operand(*op_index, dst) {
                        error!("(strenc) failed to set operand: {inst:?}");
                    }
                } else {
                    // 回写模式，需要保证字符串非常量
                    string.global.set_constant(false);
                    let flag_ptr = string.flag.unwrap_or_else(|| {
                        let value = module.add_global(i32_ty, None, ".amice_tmp_dec_flag");
                        value.set_linkage(Linkage::Private);
                        value.set_initializer(&i32_ty.const_zero());
                        value
                    });
                    let flag_ptr = flag_ptr.as_pointer_value();
                    let global_key = global_key.as_pointer_value();

                    builder.build_call(
                        decrypt_fn,
                        &[
                            ptr.into(),
                            ptr.into(),
                            len_val.into(),
                            global_key.into(),
                            flag_ptr.into(),
                        ],
                        "",
                    )?;
                }
            }
            continue;
        }

        // user_slice完全为空 -> 出现对字符串的非直接引用
        undirectly_use_strings.push(string);
    }

    if !undirectly_use_strings.is_empty() {
        emit_global_string_decryptor_ctor(module, &undirectly_use_strings, decrypt_fn, global_key)?;
    }

    Ok(())
}

fn emit_global_string_decryptor_ctor<'a>(
    module: &mut Module<'a>,
    gs: &Vec<EncryptedGlobalValue<'a>>,
    decrypt_fn: FunctionValue<'a>,
    global_key: GlobalValue,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ptr = ptr_type!(ctx, i32_type);

    let decrypt_stub_ty = ctx.void_type().fn_type(&[], false);
    let decrypt_stub = module.add_function("simd_xor_decrypt_stub", decrypt_stub_ty, None);
    decrypt_stub.set_linkage(Linkage::Internal);

    let entry = ctx.append_basic_block(decrypt_stub, "entry");
    let builder = ctx.create_builder();

    builder.position_at_end(entry);
    for string in gs {
        let ptr = if string.is_array_string() {
            // 如果是c风格的基于ArrayValue的字符串，那么全局常量本身就是这个字符串
            string.global.as_pointer_value()
        } else {
            assert!(string.is_struct_string());
            let Some(struct_value) = &string.struct_value else {
                panic!("string.struct_value must be Some(StructValue)");
            };
            let Some(field_idx) = string.field_index else {
                panic!("string.field_index must be Some(u32)");
            };
            let global_ptr = string.global.as_pointer_value();
            let value_type = string.global.get_value_type().into_struct_type();

            if log_enabled!(Level::Debug) {
                debug!(
                    "StructString found, type = {:?}, value = {:?}, idx = {}",
                    value_type, struct_value, field_idx
                );
            }

            // 生成 GEP: getelementptr inbounds %struct..., ptr @global, i32 0, i32 field_idx
            builder.build_struct_gep2(value_type, global_ptr, field_idx, "field_gep")?
        };

        let dst = ptr; // In-place decryption: src == dst
        let len_val = i32_ty.const_int(string.str_len as u64, false);
        let key_ptr = global_key.as_pointer_value();
        let flag_ptr = i32_ptr.const_null(); // No flag for global timing
        builder.build_call(
            decrypt_fn,
            &[ptr.into(), dst.into(), len_val.into(), key_ptr.into(), flag_ptr.into()],
            "",
        )?;
    }

    builder.build_return(None)?;

    let priority = 0; // Default priority
    module.append_to_global_ctors(decrypt_stub, priority);

    Ok(())
}

fn add_decrypt_function<'a>(
    module: &mut Module<'a>,
    name: &str,
    has_flag: bool,
    inline_fn: bool,
    stack_alloc: bool,
) -> anyhow::Result<FunctionValue<'a>> {
    // 密钥总是32字节的！必须是32字节的！
    // void @simd_xor_cipher(i8* src, i8* dst, i32 len, i8* key, i32* flag)
    let ctx = module.get_context();
    let i8_ty = ctx.i8_type();
    let i32_ty = ctx.i32_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ptr = ptr_type!(ctx, i32_type);
    let vector_256 = i8_ty.vec_type(32);
    let vector_ptr_type = vector_256.ptr_type(AddressSpace::default());

    let fn_ty = i8_ptr.fn_type(
        &[
            i8_ptr.into(),
            i8_ptr.into(),
            i32_ty.into(),
            i8_ptr.into(),
            i32_ptr.into(),
        ],
        false,
    );
    let decrypt_fn = module.add_function(name, fn_ty, None);

    if inline_fn {
        let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        decrypt_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
    }

    let entry = ctx.append_basic_block(decrypt_fn, "entry");
    let entry_has_flags = ctx.append_basic_block(decrypt_fn, "entry_has_flags");
    let main_loop = ctx.append_basic_block(decrypt_fn, "main_loop");
    let update_flag = ctx.append_basic_block(decrypt_fn, "update_flag");
    let key_prepare = ctx.append_basic_block(decrypt_fn, "key_prepare");
    let next = ctx.append_basic_block(decrypt_fn, "next");
    let check_rest = ctx.append_basic_block(decrypt_fn, "check_rest");
    let rest = ctx.append_basic_block(decrypt_fn, "rest");
    let exit = ctx.append_basic_block(decrypt_fn, "exit");

    let builder = ctx.create_builder();

    let src_ptr = decrypt_fn
        .get_nth_param(0)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get source pointer parameter"))?;
    let dst_ptr = decrypt_fn
        .get_nth_param(1)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get destination pointer parameter"))?;
    let len = decrypt_fn
        .get_nth_param(2)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow!("Failed to get length parameter"))?;
    let key_ptr = decrypt_fn
        .get_nth_param(3)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get key pointer parameter"))?;
    let flag_ptr = decrypt_fn
        .get_nth_param(4)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow!("Failed to get flag pointer parameter"))?;

    builder.position_at_end(entry);
    let idx = builder.build_alloca(i32_ty, "idx")?;
    builder.build_store(idx, ctx.i32_type().const_zero())?;

    if has_flag {
        let cond_if_flag_ptr_is_null = builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            flag_ptr,
            flag_ptr.get_type().const_null(),
            "",
        )?;
        builder.build_conditional_branch(cond_if_flag_ptr_is_null, key_prepare, entry_has_flags)?;

        builder.position_at_end(entry_has_flags);
        let flag = builder.build_load2(i32_ty, flag_ptr, "flag")?.into_int_value();
        let is_decrypted = builder.build_int_compare(inkwell::IntPredicate::EQ, flag, i32_ty.const_zero(), "")?;
        builder.build_conditional_branch(is_decrypted, update_flag, exit)?;
    } else {
        builder.build_unconditional_branch(key_prepare)?;
    }

    builder.position_at_end(update_flag);
    if has_flag {
        builder.build_store(flag_ptr, i32_ty.const_int(1, false))?;
    }
    builder.build_unconditional_branch(key_prepare)?;

    builder.position_at_end(key_prepare);
    let key_load_inst = builder.build_load2(vector_256, key_ptr, "key_vec")?;
    let key_vec = key_load_inst.into_vector_value();
    builder.build_unconditional_branch(main_loop)?;

    // 检查是否还有完整的32字节块
    builder.position_at_end(main_loop);
    ();
    let index = builder.build_load2(i32_ty, idx, "cur_idx")?.into_int_value();
    let tmp = builder.build_int_add(index, ctx.i32_type().const_int(31, false), "tmp")?;
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, tmp, len, "cond")?;
    builder.build_conditional_branch(cond, next, check_rest)?;

    builder.position_at_end(next);
    let src_gep = builder.build_gep2(i8_ty, src_ptr, &[index], "src_gep")?;
    let src_load_inst = builder.build_load2(vector_256, src_gep, "src_vec")?;
    if let Some(load_inst) = src_load_inst.as_instruction_value() {
        load_inst
            .set_alignment(1)
            .map_err(|e| anyhow!("Failed to set alignment for load instruction: {}", e))?;
    }
    let src_vec = src_load_inst.into_vector_value();
    let xored_vec = builder.build_xor(src_vec, key_vec, "xored_vec")?;
    let dst_gep = builder.build_gep2(i8_ty, dst_ptr, &[index], "dst_gep")?;
    let store_inst = builder.build_store(dst_gep, xored_vec)?;
    store_inst
        .set_alignment(1)
        .map_err(|e| anyhow!("Failed to set alignment for store instruction: {}", e))?;

    let next_index = builder.build_int_add(index, ctx.i32_type().const_int(32, false), "next_index")?;
    builder.build_store(idx, next_index)?;
    builder.build_unconditional_branch(main_loop)?;

    builder.position_at_end(check_rest);
    // 查询是否有剩余的字节没有处理
    let index = builder.build_load2(i32_ty, idx, "")?.into_int_value();
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond2")?;
    builder.build_conditional_branch(cond, rest, exit)?;

    builder.position_at_end(rest);
    // 处理剩余的字节
    // for(;i<len;i++) output[i] = input[i] ^ key[i % 32];

    let src_gep = builder.build_gep2(i8_ty, src_ptr, &[index], "src_rest_gep")?;
    let ch = builder.build_load2(i8_ty, src_gep, "cur")?.into_int_value();
    let key_index = builder.build_int_signed_rem(index, ctx.i32_type().const_int(32, false), "key_index")?;
    let key_gep = builder.build_gep2(i8_ty, key_ptr, &[key_index], "key_rest_gep")?;
    let key_ch = builder.build_load2(i8_ty, key_gep, "key_cur")?.into_int_value();
    let xored = builder.build_xor(ch, key_ch, "xored_rest")?;
    let dst_gep = builder.build_gep2(i8_ty, dst_ptr, &[index], "dst_rest_gep")?;
    builder.build_store(dst_gep, xored)?;

    let next_index = builder.build_int_add(index, ctx.i32_type().const_int(1, false), "next_index_rest")?;
    builder.build_store(idx, next_index)?;
    builder.build_unconditional_branch(check_rest)?;

    builder.position_at_end(exit);
    builder.build_return(Some(&dst_ptr))?;

    Ok(decrypt_fn)
}

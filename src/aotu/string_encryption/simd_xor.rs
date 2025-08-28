use crate::aotu::string_encryption::{
    EncryptedGlobalValue, STACK_ALLOC_THRESHOLD, StringEncryption, StringEncryptionAlgo, alloc_stack_string,
    array_as_const_string, collect_insert_points,
};
use crate::config::StringDecryptTiming as DecryptTiming;
use amice_llvm::inkwell2::{BuilderExt, ModuleExt};
use amice_llvm::ptr_type;
use anyhow::anyhow;
use llvm_plugin::inkwell;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, BasicValue, BasicValueEnum, FunctionValue, GlobalValue};
use log::{error, warn};
use rand::Rng;

#[derive(Default)]
pub(super) struct SimdXorAlgo {
    pub(super) key: [u8; 32],
}

impl StringEncryptionAlgo for SimdXorAlgo {
    fn initialize(&mut self, _pass: &StringEncryption, _module: &mut Module<'_>) -> anyhow::Result<()> {
        rand::rng().fill(&mut self.key);
        Ok(())
    }

    fn do_string_encrypt(&mut self, pass: &StringEncryption, module: &mut Module<'_>) -> anyhow::Result<()> {
        do_handle(pass, module, &self.key)
    }
}

fn do_handle<'a>(pass: &StringEncryption, module: &mut Module<'a>, key: &[u8; 32]) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i8_ty = ctx.i8_type();
    let vector256 = i8_ty.array_type(32);

    let is_lazy_mode = matches!(pass.timing, DecryptTiming::Lazy);
    let is_global_mode = matches!(pass.timing, DecryptTiming::Global);

    let global_key = module.add_global(vector256, Some(AddressSpace::default()), "");
    let array_values = key
        .map(|c| i8_ty.const_int(c as u64, false))
        .map(|v| unsafe { ArrayValue::new(v.as_value_ref()) });
    global_key.set_initializer(&vector256.const_array(&array_values));

    let string_global_values: Vec<EncryptedGlobalValue<'a>> = module
        .get_globals()
        .filter(|global| !matches!(global.get_linkage(), Linkage::External))
        .filter(|global| {
            (!pass.only_dot_string || global.get_name().to_str().is_ok_and(|s| s.contains(".str")))
                && global
                    .get_section()
                    .is_none_or(|section| section.to_str() != Ok("llvm.metadata"))
        })
        .filter_map(|global| match global.get_initializer()? {
            // C-like strings
            BasicValueEnum::ArrayValue(arr) => Some((global, None, arr)),
            // Rust-like strings
            BasicValueEnum::StructValue(stru) if stru.count_fields() <= 1 => match stru.get_field_at_index(0)? {
                BasicValueEnum::ArrayValue(arr) => Some((global, Some(stru), arr)),
                _ => None,
            },
            _ => None,
        })
        .filter(|(_, _, arr)| {
            if !arr.is_const_string() {
                return false;
            }

            let ty = arr.get_type();
            !ty.is_empty() && ty.len() > 1
        })
        .filter_map(|(global, stru, arr)| {
            let s = array_as_const_string(&arr).and_then(|s| str::from_utf8(s).ok())?;
            let mut encoded_str = vec![0u8; s.len()];
            for (i, c) in s.bytes().enumerate() {
                encoded_str[i] = c ^ key[i % key.len()];
            }

            let unique_name = global
                .get_name()
                .to_str()
                .map_or_else(|_| rand::random::<u32>().to_string(), |s| s.to_string());
            Some((unique_name, global, stru, encoded_str))
        })
        .map(|(unique_name, global, stru, encoded_str)| {
            let string_len = encoded_str.len() as u32;
            let mut should_use_stack = pass.stack_alloc && string_len <= STACK_ALLOC_THRESHOLD;

            // Warn if stack allocation is requested but string is too large
            if pass.stack_alloc && string_len > STACK_ALLOC_THRESHOLD {
                warn!(
                    "(strenc) string '{}' ({}B) exceeds 4KB limit for stack allocation, using global timing instead",
                    unique_name, string_len
                );
            }

            if let Some(stru) = stru {
                // Rust-like strings
                let new_const = ctx.const_string(&encoded_str, false);
                stru.set_field_at_index(0, new_const);
                global.set_initializer(&stru);
            } else {
                // C-like strings
                let new_const = ctx.const_string(&encoded_str, false);
                global.set_initializer(&new_const);
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

                    users.append(&mut temp_user);
                }

                if users.is_empty() {
                    // 找不到调用点的字符串
                    should_use_stack = false;
                }
            }

            // 当需要回写解密的时候，一个flag是必须的，虽然我没有办法保证线程安全（这也许是一个todo
            // 什么时候是回写解密？是懒加载开启且不是在栈上解密的情况下！
            // 全局函数解密不需要flag，省去这个步骤
            let flag = if is_lazy_mode && !should_use_stack {
                let flag = module.add_global(i32_ty, None, &format!("dec_flag_{unique_name}"));
                flag.set_initializer(&i32_ty.const_int(0, false));
                flag.set_linkage(Linkage::Internal);
                Some(flag)
            } else {
                None
            };

            // 如果有flag的话 ===> 回写模式，字符串不能是一个常量
            if !flag.is_none() || is_global_mode {
                global.set_constant(false);
            }

            EncryptedGlobalValue::new(global, string_len, flag, should_use_stack, users)
        })
        .collect();

    let decrypt_fn = add_decrypt_function(
        module,
        &format!("simd_xor_cipher_{}", rand::random::<u32>()),
        is_lazy_mode,
        pass.inline_decrypt,
        pass.stack_alloc,
    )?;

    match pass.timing {
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
                    pass.allow_non_entry_stack_alloc,
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
                builder.position_before(inst);

                let ptr = string.global.as_pointer_value();
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
    for ev in gs {
        let ptr = ev.global.as_pointer_value();
        let dst = ev.global.as_pointer_value(); // In-place decryption: src == dst
        let len_val = i32_ty.const_int(ev.str_len as u64, false);
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
    let i32_one = i32_ty.const_int(1, false);

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
    let index = builder.build_int_sub(len, i32_one, "")?;
    let null_gep = builder.build_gep2(i8_ty, dst_ptr, &[index], "null_gep")?;
    builder.build_store(null_gep, i8_ty.const_zero())?;
    builder.build_return(Some(&dst_ptr))?;

    Ok(decrypt_fn)
}

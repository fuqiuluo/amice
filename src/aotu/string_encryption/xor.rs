use crate::aotu::string_encryption::{
    EncryptedGlobalValue, STACK_ALLOC_THRESHOLD, StringEncryption, StringEncryptionAlgo, alloc_stack_string,
    array_as_const_string, collect_insert_points,
};
use crate::config::StringDecryptTiming as DecryptTiming;
use amice_llvm::inkwell2::AdvancedInkwellBuilder;
use amice_llvm::module_utils::append_to_global_ctors;
use amice_llvm::ptr_type;
use inkwell::module::Module;
use inkwell::values::FunctionValue;
use llvm_plugin::inkwell;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::module::Linkage;
use llvm_plugin::inkwell::values::{BasicValue, BasicValueEnum};
use log::{debug, error, warn};

#[derive(Default)]
pub(super) struct XorAlgo;

impl StringEncryptionAlgo for XorAlgo {
    fn initialize(&mut self, _pass: &StringEncryption, _module: &mut Module<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn do_string_encrypt(&mut self, pass: &StringEncryption, module: &mut Module<'_>) -> anyhow::Result<()> {
        do_handle(pass, module)
    }
}

fn do_handle<'a>(pass: &StringEncryption, module: &mut Module<'a>) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();

    let is_lazy_mode = matches!(pass.timing, DecryptTiming::Lazy);
    let is_global_mode = matches!(pass.timing, DecryptTiming::Global);

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
            // we ignore non-UTF8 strings, since they are probably not human-readable
            let s = array_as_const_string(&arr).and_then(|s| str::from_utf8(s).ok())?;

            let mut encoded_str = s.bytes().collect::<Vec<_>>();
            for byte in encoded_str.iter_mut() {
                *byte ^= 0xAA;
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

            let mut users = Vec::new();

            if !is_global_mode && (is_lazy_mode) {
                // 获取这个字符串的调用者，并尝试收集该调用者的插入点，方便懒加载的初始化
                let mut use_opt = global.get_first_use();
                while let Some(u) = use_opt {
                    use_opt = u.get_next_use();
                    // 这里有情况就是无法获取到具体的插入点
                    // @.str = private unnamed_addr constant [3 x i8] c"\8F\D9\AA", align 1
                    // @S_BRANCH_A = internal global ptr @.str.12, align 8
                    // 比如说这种，一个全局字符串指向这个字符串常量
                    // 他的user就是%7 = load ptr, ptr @S_BRANCH_A, align 8，对于这种情况建议下降到回写解密

                    let mut temp_user = Vec::new();
                    // 只收集直接引用字符串的插入点
                    if let Err(e) = collect_insert_points(global, u.get_user(), &mut temp_user) {
                        error!("(strenc) failed to collect insert points: {e}");
                    }

                    // 这里出现了这个字符串获取不到插入点的情况，这里是只要任何一个调用者获取不到插入点，整体就下降到回写解密（全局函数内）
                    // 为什么是任意一个调用者获取不到就进入下降？因为做局部回写很麻烦直接改成整个字符串加密都回写解密
                    // @.str = private unnamed_addr constant [3 x i8] c"\8F\D9\AA", align 1
                    // @S_BRANCH_A = internal global ptr @.str.12, align 8 <-- 调用者，但是获取不到插入点（其实可以获取，不过太麻烦了）
                    // %28 = call i32 (ptr, ...) @printf(ptr noundef @.str.3) <-- 调用者，可以获取到插入点
                    if temp_user.is_empty() {
                        debug!("(strenc) failed to collect insert points for {:?}", u.get_user());
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

            // 如果有flag的话 ===> 回写模式，字符串不能是一个常量
            if !flag.is_none() || is_global_mode {
                global.set_constant(false);
            }

            EncryptedGlobalValue::new(global, string_len, flag, should_use_stack, users)
        })
        .collect();

    // 统一解密函数
    let decrypt_fn = add_decrypt_function(
        module,
        &format!("__amice__decrypt_strings_{}__", rand::random::<u32>()),
        is_lazy_mode,
        pass.inline_decrypt, // 这个inline虽然是设置`总是`，但是不一定成功？
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
                )?;
            }
            if !write_back_strings.is_empty() {
                emit_decrypt_before_inst(module, write_back_strings, decrypt_fn, false, false)?;
            }
        },
        DecryptTiming::Global => emit_global_string_decryptor_ctor(module, &string_global_values, decrypt_fn)?,
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
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i32_ptr = ptr_type!(ctx, i32_type);

    let mut undirectly_use_strings = Vec::new();
    for string in strings {
        assert_eq!(stack_alloc, string.use_stack_alloc);

        // 保险一点，不是栈上解密就是回写模式，需要非常量
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
                    builder.build_call(
                        decrypt_fn,
                        &[ptr.into(), len_val.into(), flag_ptr.into(), dst.into()],
                        "",
                    )?;

                    if !inst.set_operand(*op_index, dst) {
                        error!("(strenc) failed to set operand: {inst:?}");
                    }
                } else {
                    // 回写模式，需要保证字符串非常量
                    string.global.set_constant(false);
                    let flag_ptr = string.flag.unwrap_or_else(|| {
                        // 居然没有flag？？？？？？现场生成一个，防止崩溃
                        let value = module.add_global(i32_ty, None, ".amice_tmp_dec_flag");
                        value.set_linkage(Linkage::Private);
                        value.set_initializer(&i32_ty.const_zero());
                        value
                    });

                    builder.build_call(
                        decrypt_fn,
                        &[
                            ptr.into(),
                            len_val.into(),
                            flag_ptr.as_pointer_value().into(),
                            ptr.into(),
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
        emit_global_string_decryptor_ctor(module, &undirectly_use_strings, decrypt_fn)?;
    }

    Ok(())
}

fn emit_global_string_decryptor_ctor<'a>(
    module: &mut Module<'a>,
    global_strings: &Vec<EncryptedGlobalValue<'a>>,
    decrypt_fn: FunctionValue<'a>,
) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i32_ptr = ptr_type!(ctx, i32_type);

    let decrypt_stub_ty = ctx.void_type().fn_type(&[], false);
    let decrypt_stub = module.add_function("decrypt_strings_stub", decrypt_stub_ty, None);
    decrypt_stub.set_linkage(Linkage::Internal);

    let entry = ctx.append_basic_block(decrypt_stub, "entry");
    let builder = ctx.create_builder();

    builder.position_at_end(entry);
    for string in global_strings {
        assert!(!string.use_stack_alloc);

        let ptr = string.global.as_pointer_value();
        let len_val = i32_ty.const_int(string.str_len as u64, false);
        let flag_ptr = i32_ptr.const_null();
        let dst = ptr;
        builder.build_call(
            decrypt_fn,
            &[ptr.into(), len_val.into(), flag_ptr.into(), dst.into()],
            "",
        )?;
    }

    builder.build_return(None)?;

    let priority = 0; // Default priority
    append_to_global_ctors(module, decrypt_stub, priority);

    Ok(())
}

fn add_decrypt_function<'a>(
    module: &mut Module<'a>,
    name: &str,
    has_flag: bool,
    inline_fn: bool,
) -> anyhow::Result<FunctionValue<'a>> {
    let ctx = module.get_context();
    let i8_ty = ctx.i8_type();
    let i32_ty = ctx.i32_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ptr = ptr_type!(ctx, i32_type);
    let i32_one = i32_ty.const_int(1, false);

    // void decrypt_strings(i8* str, i32 len, i32* flag, i8* dst)
    let fn_ty = ctx
        .void_type()
        .fn_type(&[i8_ptr.into(), i32_ty.into(), i32_ptr.into(), i8_ptr.into()], false);

    let decrypt_fn = module.add_function(name, fn_ty, None);
    decrypt_fn.set_linkage(Linkage::Internal);
    if inline_fn {
        warn!("(strenc) using inline decryption function, this may increase binary size.");
        let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        decrypt_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
    }

    let prepare = ctx.append_basic_block(decrypt_fn, "prepare");
    let prepare_has_flags = if has_flag {
        ctx.append_basic_block(decrypt_fn, "prepare_has_flags").into()
    } else {
        None
    };
    let update_flag = if has_flag {
        ctx.append_basic_block(decrypt_fn, "update_flag").into()
    } else {
        None
    };
    let entry = ctx.append_basic_block(decrypt_fn, "entry");
    let body = ctx.append_basic_block(decrypt_fn, "body");
    let next = ctx.append_basic_block(decrypt_fn, "next");
    let exit = ctx.append_basic_block(decrypt_fn, "exit");

    let ptr = decrypt_fn
        .get_nth_param(0)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get pointer parameter"))?;
    let len = decrypt_fn
        .get_nth_param(1)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get length parameter"))?;
    let flag_ptr = decrypt_fn
        .get_nth_param(2)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get flag parameter"))?;
    let dst_ptr = decrypt_fn
        .get_nth_param(3)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get source pointer parameter"))?;

    let builder = ctx.create_builder();
    builder.position_at_end(prepare);

    if has_flag && let Some(prepare_has_flags) = prepare_has_flags {
        let cond = builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            flag_ptr,
            flag_ptr.get_type().const_null(),
            "has_flag",
        )?;
        builder.build_conditional_branch(cond, entry, prepare_has_flags)?;

        builder.position_at_end(prepare_has_flags);
        let flag = builder.build_load2(i32_ty, flag_ptr, "flag")?.into_int_value();
        let is_decrypted =
            builder.build_int_compare(inkwell::IntPredicate::EQ, flag, i32_ty.const_zero(), "is_decrypted")?;
        builder.build_conditional_branch(is_decrypted, update_flag.unwrap(), exit)?;
    } else {
        builder.build_unconditional_branch(entry)?;
    }

    if has_flag && let Some(update_flag) = update_flag {
        builder.position_at_end(update_flag);
        builder.build_store(flag_ptr, i32_ty.const_int(1, false))?;
        builder.build_unconditional_branch(entry)?;
    }

    builder.position_at_end(entry);
    let idx = builder.build_alloca(i32_ty, "idx")?;
    builder.build_store(idx, ctx.i32_type().const_zero())?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(body);
    let index = builder.build_load2(i32_ty, idx, "cur_idx")?.into_int_value();
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond")?;
    builder.build_conditional_branch(cond, next, exit)?;

    builder.position_at_end(next);

    // 从源地址读取
    let src_gep = builder.build_gep2(i8_ty, ptr, &[index], "src_gep")?;
    let ch = builder.build_load2(i8_ty, src_gep, "cur")?.into_int_value();
    // 解密
    let xor_ch = i8_ty.const_int(0xAA, false);
    let xored = builder.build_xor(ch, xor_ch, "new")?;
    // 写入目标地址（栈上）
    let dst_gep = builder.build_gep2(i8_ty, dst_ptr, &[index], "dst_gep")?;
    builder.build_store(dst_gep, xored)?;

    let next_index = builder.build_int_add(index, ctx.i32_type().const_int(1, false), "")?;
    builder.build_store(idx, next_index)?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(exit);
    let index = builder.build_int_sub(len, i32_one, "")?;
    let null_gep = builder.build_gep2(i8_ty, dst_ptr, &[index], "null_gep")?;
    builder.build_store(null_gep, i8_ty.const_zero())?;
    builder.build_return(None)?;

    Ok(decrypt_fn)
}

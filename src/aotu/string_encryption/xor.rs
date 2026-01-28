use crate::aotu::string_encryption::{
    EncryptedGlobalValue, STACK_ALLOC_THRESHOLD, StringEncryption, StringEncryptionAlgo, alloc_stack_string,
    array_as_const_string, collect_insert_points,
};
use crate::config::{StringDecryptTiming as DecryptTiming, StringEncryptionConfig};
use amice_llvm::inkwell2::{BasicBlockExt, BuilderExt, LLVMValueRefExt, ModuleExt};
use amice_llvm::ptr_type;
use inkwell::GlobalVisibility;
use inkwell::module::Module;
use inkwell::values::{FunctionValue, InstructionOpcode};
use llvm_plugin::inkwell;
use llvm_plugin::inkwell::AddressSpace;
use llvm_plugin::inkwell::AtomicOrdering;
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::comdat::Comdat;
use llvm_plugin::inkwell::module::Linkage;
use llvm_plugin::inkwell::values::{AsValueRef, BasicValue, BasicValueEnum};
use log::{Level, debug, error, info, log_enabled, warn};
use std::ptr::{null, null_mut};

#[derive(Default)]
pub(super) struct XorAlgo;

impl StringEncryptionAlgo for XorAlgo {
    fn initialize(&mut self, _cfg: &StringEncryptionConfig, _module: &mut Module<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    fn do_string_encrypt(&mut self, cfg: &StringEncryptionConfig, module: &mut Module<'_>) -> anyhow::Result<()> {
        do_handle(cfg, module)
    }
}

fn do_handle<'a>(cfg: &StringEncryptionConfig, module: &mut Module<'a>) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();

    let is_lazy_mode = matches!(cfg.timing, DecryptTiming::Lazy);
    let is_global_mode = matches!(cfg.timing, DecryptTiming::Global);

    let all_globals: Vec<_> = module.get_globals().collect();
    if log_enabled!(Level::Debug) {
        info!("(strenc) total globals in module: {}", all_globals.len());
        for g in &all_globals {
            info!("(strenc) global in module: {:?}", g.get_name());
        }
    }

    let string_global_values: Vec<EncryptedGlobalValue<'a>> = all_globals
        .into_iter()
        .inspect(|global| {
            if log_enabled!(Level::Debug) {
                info!(
                    "(strenc) checking global: {:?}, linkage: {:?}",
                    global.get_name(),
                    global.get_linkage()
                );
            }
        })
        .filter(|global| !matches!(global.get_linkage(), Linkage::External))
        .filter(|global| {
            let name_ok = !cfg.only_dot_str || global.get_name().to_str().is_ok_and(|s| s.contains(".str"));
            let section_ok = global
                .get_section()
                .is_none_or(|section| section.to_str() != Ok("llvm.metadata"));
            if !name_ok && log_enabled!(Level::Debug) {
                info!(
                    "(strenc) skipping {:?}: name filter (only_dot_str={})",
                    global.get_name(),
                    cfg.only_dot_str
                );
            }
            if !section_ok && log_enabled!(Level::Debug) {
                info!("(strenc) skipping {:?}: section filter", global.get_name());
            }
            name_ok && section_ok
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
                if arr.is_undef() || arr.is_null() {
                    debug!(
                        "(strenc) skipping field in {:?}: is_undef() or is_null()",
                        global.get_name()
                    );
                    return None;
                }

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
            // we ignore non-UTF8 strings, since they are probably not human-readable
            let s = array_as_const_string(&arr)?.to_vec();

            if log_enabled!(Level::Debug) {
                info!(
                    "Will process string {:?}: {}, len = {}",
                    global,
                    hex::encode(&s),
                    s.len()
                )
            }

            let mut encoded_str = s;
            for byte in encoded_str.iter_mut() {
                *byte ^= 0xAA;
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

            // 判断是不是C like的基于ArrayValue的字符串
            // 如果为false就是Rust/C++ NTTP的string, 这个时候一个结构体里面可能出现多个字符串
            // 需要struct gep回去结构体底下指向字符串的指针，而不是指向结构体的头
            let mut is_array_string = true;
            if let Some(stru) = stru {
                // Rust-like strings or C++ NTTP string
                let mut values: Vec<_> = stru.get_fields().collect();
                let new_const = ctx.const_string(&encoded_str, false);
                if let Some(field_idx) = field_idx {
                    is_array_string = false;
                    values[field_idx as usize] = BasicValueEnum::ArrayValue(new_const);
                } else {
                    values[0] = BasicValueEnum::ArrayValue(new_const);
                }

                let stru = stru.get_type().const_named_struct(values.as_slice());
                global.set_initializer(&stru);

                let current_linkage = global.get_linkage();
                if matches!(
                    current_linkage,
                    Linkage::LinkOnceAny | Linkage::LinkOnceODR | Linkage::WeakAny | Linkage::WeakODR
                ) {
                    global.set_linkage(Linkage::Internal);
                    global.set_comdat(unsafe { Comdat::new(null_mut()) });
                    global.set_visibility(GlobalVisibility::Default);
                }
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

    if log_enabled!(Level::Debug) {
        debug!("strings count: {}", string_global_values.len());
    }

    // 统一解密函数
    let decrypt_fn = add_decrypt_function(
        module,
        &format!("__amice__decrypt_strings_{}__", rand::random::<u32>()),
        is_lazy_mode,
        cfg.inline_decrypt, // 这个inline虽然是设置`总是`，但是不一定成功？
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
                emit_decrypt_before_inst(module, stack_strings, decrypt_fn, true, cfg.allow_non_entry_stack_alloc)?;
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
                // 如果指令是PHI节点，需要在基本块的第一个非PHI指令位置插入解密代码
                // PHI节点必须在基本块开头，不能在PHI节点前插入任何非PHI指令
                let insert_point = if inst.get_opcode() == InstructionOpcode::Phi {
                    // 一般来说没有问题吧? (((有没有可能出现这种情况呢？
                    // %1 = phi ....
                    // call void xxx(%1)
                    // %2 = phi ....
                    // 不能吧? 那就真不能，出现了那就真倒糙了
                    if let Some(parent_bb) = inst.get_parent() {
                        parent_bb.get_first_insertion_pt()
                    } else {
                        error!("(strenc) PHI instruction has no parent block: {inst:?}");
                        *inst
                    }
                } else {
                    *inst
                };

                // 去到梦开始的地方!!!
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
                    builder.build_call(
                        decrypt_fn,
                        &[ptr.into(), len_val.into(), flag_ptr.into(), dst.into()],
                        "",
                    )?;

                    // 如果是phi，那就没办法替换了，因为这样会出现因果倒置
                    // %1 = phi @.str -> %1 = phi %2 这样生命周期就不对了喵
                    // %2 = alloc 1000000TB
                    // call void dec_str(@.str, %2)
                    // printf(%1)
                    // 这个时候我们有两个解决方案：
                    // ----- 如果字符串有phi user，不允许该字符串使用栈分配
                    // ----- phi指令的字符串，把phi调用点全replace了，但是貌似不可行，因为会出现一个极端情况：
                    // ---------- 有二货把phi出来的指针保存全局变量？栈分内存不能保存全局的！！

                    if !inst.set_operand(*op_index, dst) {
                        error!("(strenc) failed to set operand: {inst:?}");
                    }
                } else {
                    // 回写模式，需要保证字符串非常量
                    string.global.set_constant(false);
                    let flag_ptr = string.flag.unwrap_or_else(|| {
                        // 居然没有flag? wtf? 现场生成一个，防止崩溃?
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
    module.append_to_global_ctors(decrypt_stub, priority);

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
    let spin_wait = if has_flag {
        ctx.append_basic_block(decrypt_fn, "spin_wait").into()
    } else {
        None
    };
    let do_mark_done = if has_flag {
        ctx.append_basic_block(decrypt_fn, "do_mark_done").into()
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

    if has_flag
        && let Some(prepare_has_flags) = prepare_has_flags
        && let Some(spin_wait) = spin_wait
        && let Some(do_mark_done) = do_mark_done
    {
        // Check if flag_ptr is NULL (stack allocation case)
        let flag_is_null = builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            flag_ptr,
            flag_ptr.get_type().const_null(),
            "flag_is_null",
        )?;
        // If flag_ptr is NULL, go directly to decrypt (no synchronization needed)
        // If flag_ptr is not NULL, go to prepare_has_flags for synchronization
        builder.build_conditional_branch(flag_is_null, entry, prepare_has_flags)?;

        builder.position_at_end(prepare_has_flags);
        // Three-state protocol for thread safety:
        // 0 = not decrypted, 1 = decrypting (in progress), 2 = decrypted (complete)
        // Try to atomically change flag from 0 to 1 (claim the decryption task)
        let cmpxchg_result = builder.build_cmpxchg(
            flag_ptr,
            i32_ty.const_zero(),        // expected: 0 (not decrypted)
            i32_ty.const_int(1, false), // new value: 1 (decrypting)
            AtomicOrdering::AcquireRelease,
            AtomicOrdering::Acquire,
        )?;
        // Extract the success flag (second element of the result struct)
        let cmpxchg_success = builder
            .build_extract_value(cmpxchg_result, 1, "cmpxchg_success")?
            .into_int_value();
        // If cmpxchg succeeded (we are the winner), proceed to decrypt
        // If cmpxchg failed (someone else is decrypting or already done), go to spin_wait
        builder.build_conditional_branch(cmpxchg_success, entry, spin_wait)?;

        // Spin-wait loop: wait until flag becomes 2 (decryption complete)
        builder.position_at_end(spin_wait);
        let flag_val = builder.build_load2(i32_ty, flag_ptr, "flag_val")?.into_int_value();
        // Make the load atomic to ensure we see the latest value
        if let Some(load_inst) = flag_val.as_instruction_value() {
            load_inst.set_atomic_ordering(AtomicOrdering::Acquire)?;
        }
        let is_complete = builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            flag_val,
            i32_ty.const_int(2, false), // 2 = decrypted (complete)
            "is_complete",
        )?;
        // If complete, exit; otherwise keep spinning
        builder.build_conditional_branch(is_complete, exit, spin_wait)?;

        // Mark decryption as complete (flag = 2)
        // This block is only reached after successful decryption
        builder.position_at_end(do_mark_done);
        let store_inst = builder.build_store(flag_ptr, i32_ty.const_int(2, false))?;
        store_inst.set_atomic_ordering(AtomicOrdering::Release)?;
        builder.build_unconditional_branch(exit)?;
    } else {
        builder.build_unconditional_branch(entry)?;
    }

    builder.position_at_end(entry);
    let idx = builder.build_alloca(i32_ty, "idx")?;
    builder.build_store(idx, ctx.i32_type().const_zero())?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(body);
    let index = builder.build_load2(i32_ty, idx, "cur_idx")?.into_int_value();
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond")?;
    // When done decrypting:
    // - If we came from prepare_has_flags path (flag_ptr not NULL), go to do_mark_done
    // - If we came from prepare path (flag_ptr NULL), go directly to exit
    // We detect this by checking if flag_ptr is NULL again
    let done_block = if has_flag && do_mark_done.is_some() {
        // Build a check at the end of decryption loop
        let check_block = ctx.append_basic_block(decrypt_fn, "check_mark_done");
        builder.build_conditional_branch(cond, next, check_block)?;

        builder.position_at_end(check_block);
        let flag_is_null = builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            flag_ptr,
            flag_ptr.get_type().const_null(),
            "flag_is_null_exit",
        )?;
        builder.build_conditional_branch(flag_is_null, exit, do_mark_done.unwrap())?;

        // Return a dummy - we've already built the branch
        None
    } else {
        Some(exit)
    };

    if let Some(done_block) = done_block {
        builder.build_conditional_branch(cond, next, done_block)?;
    }

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
    builder.build_return(None)?;

    Ok(decrypt_fn)
}

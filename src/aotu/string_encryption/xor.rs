use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use ascon_hash::Digest;
use inkwell::module::Module;
use inkwell::values::FunctionValue;
use llvm_plugin::inkwell::{AddressSpace, Either};
use llvm_plugin::{inkwell, FunctionAnalysisManager, ModuleAnalysisManager};
use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
use llvm_plugin::inkwell::basic_block::BasicBlock;
use llvm_plugin::inkwell::context::ContextRef;
use llvm_plugin::inkwell::module::Linkage;
use llvm_plugin::inkwell::values::{AnyValueEnum, AsValueRef, BasicValue, BasicValueEnum, BasicValueUse, GlobalValue, InstructionValue};
use log::{debug, error, info, warn};
use crate::aotu::string_encryption::{array_as_const_string, DecryptTiming, EncryptedGlobalValue, StringEncryption};
use crate::ptr_type;

pub(crate) fn do_handle<'a>(pass: &StringEncryption, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();

    let has_flag = matches!(pass.decrypt_timing, DecryptTiming::Lazy);

    let gs: Vec<EncryptedGlobalValue<'a>> = module.get_globals()
        .filter(|global| !matches!(global.get_linkage(), Linkage::External))
        .filter(|global| {
            global.get_section().map_or(true, |section| {
                section.to_str().map_or(true, |s| s != "llvm.metadata")
            })
        })
        .filter_map(|global| match global.get_initializer()? {
            // C-like strings
            BasicValueEnum::ArrayValue(arr) => Some((global, None, arr)),
            // Rust-like strings
            BasicValueEnum::StructValue(stru) if stru.count_fields() <= 1 => {
                match stru.get_field_at_index(0)? {
                    BasicValueEnum::ArrayValue(arr) => Some((global, Some(stru), arr)),
                    _ => None,
                }
            }
            _ => None,
        })
        .filter(|(_, _, arr)| {
            // needs to be called before `array_as_const_string`, otherwise it may crash
            arr.is_const_string()
        })
        .filter_map(|(global, stru, arr)| {
            // we ignore non-UTF8 strings, since they are probably not human-readable
            let s = array_as_const_string(&arr).and_then(|s| str::from_utf8(s).ok())?;
            let encoded_str = s.bytes().map(|c| c ^ 0xAA).collect::<Vec<_>>();
            let unique_name = global.get_name().to_str()
                .map_or_else(|_| rand::random::<u32>().to_string(), |s| s.to_string());
            Some((unique_name, global, stru, encoded_str))
        })
        .map(|(unique_name, global, stru, encoded_str)| {
            let flag = if has_flag {
                let flag = module.add_global(i32_ty, None, &format!("dec_flag_{}", unique_name));
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
                if !pass.stack_alloc {
                    global.set_constant(false);
                }
                EncryptedGlobalValue {
                    global,
                    len: encoded_str.len() as u32,
                    flag,
                    oneshot: false,
                }
            } else {
                // C-like strings
                let new_const = ctx.const_string(&encoded_str, false);
                global.set_initializer(&new_const);
                if !pass.stack_alloc {
                    global.set_constant(false);
                }
                EncryptedGlobalValue {
                    global,
                    len: encoded_str.len() as u32,
                    flag,
                    oneshot: false,
                }
            }
        })
        .collect();

    let decrypt_fn = if pass.stack_alloc {
        warn!("(strenc) using stack allocation for decryption, this may cause issues with large strings or in multi-threaded contexts.");
        add_decrypt_function_stack(
            module,
            &format!("decrypt_strings_stack_{}", rand::random::<u32>()),
            pass.inline_decrypt,
        )?
    } else {
        add_decrypt_function(
            module,
            &format!("decrypt_strings_{}", rand::random::<u32>()),
            has_flag,
            pass.inline_decrypt
        )?
    };

    match pass.decrypt_timing {
        DecryptTiming::Lazy => do_lazy(&gs, decrypt_fn, ctx, pass.stack_alloc)?,
        DecryptTiming::Global => {
            assert!(!pass.stack_alloc, "(strenc) global decrypt timing is not supported with stack allocation");
            do_global(module, &gs, decrypt_fn, ctx)?
        }
    }

    Ok(())
}

fn do_lazy(
    gs: &[EncryptedGlobalValue],
    decrypt_fn: FunctionValue<'_>,
    ctx: ContextRef,
    stack_alloc: bool,
) -> anyhow::Result<()> {
    let insert_fn = if stack_alloc {
        insert_decrypt_stack_call
    } else {
        insert_decrypt_call
    };

    for ev in gs {
        let mut uses = Vec::new();
        let mut use_opt = ev.global.get_first_use();
        while let Some(u) = use_opt {
            use_opt = u.get_next_use();
            uses.push(u);
        }

        for u in uses {
            match u.get_user() {
                AnyValueEnum::InstructionValue(inst) => insert_fn(
                    ctx,
                    inst,
                    &ev.global,
                    decrypt_fn,
                    ev.len,
                    ev.flag,
                )?,
                AnyValueEnum::IntValue(value) => {
                    if let Some(inst) = value.as_instruction_value() {
                        insert_fn(
                            ctx,
                            inst,
                            &ev.global,
                            decrypt_fn,
                            ev.len,
                            ev.flag,
                        )?
                    } else {
                        error!("(strenc) unexpected IntValue user: {:?}", value);
                    }
                }
                AnyValueEnum::PointerValue(gv) => {
                    if let Some(inst) = gv.as_instruction_value() {
                        insert_fn(
                            ctx,
                            inst,
                            &ev.global,
                            decrypt_fn,
                            ev.len,
                            ev.flag,
                        )?
                    } else {
                        error!("(strenc) unexpected PointerValue user: {:?}", gv);
                    }
                }
                _ => {
                    error!("(strenc) unexpected user type: {:?}", u.get_user());
                }
            }
        }
    }

    Ok(())
}

fn insert_decrypt_stack_call<'a>(
    ctx: ContextRef<'a>,
    inst: InstructionValue<'a>,
    global: &GlobalValue<'a>,
    decrypt_fn: FunctionValue<'a>,
    len: u32,
    flag: Option<GlobalValue<'a>>,
) -> anyhow::Result<()> {
    let i32_ty = ctx.i32_type();
    let i8_ty = ctx.i8_type();

    let parent_bb = inst.get_parent().expect("inst must be in a block");
    let parent_fn = parent_bb.get_parent().expect("block must have parent fn");
    //info!("(strenc) inserting decrypt_stack call for global: {:?}", global.get_name());

    let builder = ctx.create_builder();
    builder.position_before(&inst);

    let container = builder.build_array_alloca(
        i8_ty,
        i32_ty.const_int(len as u64 + 1, false),
        ""
    )?;
    let src_ptr = global.as_pointer_value();
    let len_val = i32_ty.const_int(len as u64, false);

    //debug!("(strenc) stack alloc: {:?}", container);
    //debug!("(strenc) decrypt_fn: {:?}", decrypt_fn);
    //debug!("(strenc) inst: {:?}", inst);

    let decrypted_ptr = builder.build_call(
        decrypt_fn,
        &[src_ptr.into(), len_val.into(), container.into()],
        ""
    )?.try_as_basic_value().left().unwrap().into_pointer_value();

    let mut replaced = false;
    for i in 0..inst.get_num_operands() {
        if let Some(op) = inst.get_operand(i) {
            if let Some(operand) = op.left() {
                if operand.as_value_ref() == global.as_value_ref() {
                    inst.set_operand(i, decrypted_ptr.as_basic_value_enum());
                    replaced = true;
                    break;
                }
            }
        }
    }

    if !replaced {
        error!("(strenc) failed to replace global operand in instruction: {:?}", inst);
    }

    Ok(())
}

fn insert_decrypt_call<'a>(
    ctx: ContextRef<'a>,
    inst: InstructionValue<'a>,
    global: &GlobalValue<'a>,
    decrypt_fn: FunctionValue<'a>,
    len: u32,
    flag: Option<GlobalValue<'a>>,
) -> anyhow::Result<()> {
    let i32_ty = ctx.i32_type();
    let parent_bb = inst.get_parent().expect("inst must be in a block");
    let parent_fn = parent_bb.get_parent().expect("block must have parent fn");

    let builder = ctx.create_builder();
    builder.position_before(&inst);
    let ptr = global.as_pointer_value();
    let len_val = i32_ty.const_int(len as u64, false);
    let flag_ptr = flag.unwrap().as_pointer_value();
    builder.build_call(decrypt_fn, &[ptr.into(), len_val.into(), flag_ptr.into()], "", )?;

    Ok(())
}

fn add_decrypt_function<'a>(
    module: &mut Module<'a>,
    name: &str,
    has_flag: bool,
    inline_fn: bool
) -> anyhow::Result<FunctionValue<'a>> {
    let ctx = module.get_context();
    let i8_ty  = ctx.i8_type();
    let i32_ty = ctx.i32_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ptr = ptr_type!(ctx, i32_type);

    // void decrypt_strings(i8* str, i32 len, i32* flag)
    let fn_ty = ctx.void_type()
        .fn_type(&[i8_ptr.into(), i32_ty.into(), i32_ptr.into()], false);

    let decrypt_fn = module.add_function(name, fn_ty, None);
    decrypt_fn.set_linkage(Linkage::Internal);
    if inline_fn {
        warn!("(strenc) using inline decryption function, this may increase binary size.");
        let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        decrypt_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
    }

    let prepare = ctx.append_basic_block(decrypt_fn, "prepare");
    let entry = ctx.append_basic_block(decrypt_fn, "entry");
    let body = ctx.append_basic_block(decrypt_fn, "body");
    let next = ctx.append_basic_block(decrypt_fn, "next");
    let exit = ctx.append_basic_block(decrypt_fn, "exit");

    let builder = ctx.create_builder();
    builder.position_at_end(prepare);
    let flag_ptr = decrypt_fn.get_nth_param(2)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get flag parameter"))?;
    if has_flag {
        let flag = builder.build_load(i32_ty, flag_ptr, "flag")?.into_int_value();
        let is_decrypted = builder.build_int_compare(inkwell::IntPredicate::EQ, flag, i32_ty.const_zero(), "is_decrypted")?;
        builder.build_conditional_branch(is_decrypted, entry, exit)?;
    } else {
        builder.build_unconditional_branch(entry)?;
    }

    builder.position_at_end(entry);
    if has_flag {
        builder.build_store(flag_ptr, i32_ty.const_int(1, false))?;
    }

    let idx = builder.build_alloca(i32_ty, "idx")?;
    builder.build_store(idx, ctx.i32_type().const_zero())?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(body);
    let index = builder.build_load(i32_ty, idx, "cur_idx")?.into_int_value();
    let len = decrypt_fn.get_nth_param(1)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get length parameter"))?;
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond")?;
    builder.build_conditional_branch(cond, next, exit)?;

    builder.position_at_end(next);
    let ptr = decrypt_fn.get_nth_param(0)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get pointer parameter"))?;
    let gep = unsafe {
        builder.build_gep(i8_ty, ptr, &[index], "gep")
    }?;
    let ch = builder.build_load(i8_ty, gep, "cur")?.into_int_value();
    let xor_ch = i8_ty.const_int(0xAA, false);
    let xored = builder.build_xor(ch, xor_ch, "new")?;
    builder.build_store(gep, xored)?;
    let next_index = builder.build_int_add(index, ctx.i32_type().const_int(1, false), "")?;
    builder.build_store(idx, next_index)?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(exit);
    builder.build_return(None)?;

    Ok(decrypt_fn)
}

fn add_decrypt_function_stack<'a>(
    module: &mut Module<'a>,
    name: &str,
    inline_fn: bool
) -> anyhow::Result<FunctionValue<'a>> {
    let ctx = module.get_context();
    let i8_ty  = ctx.i8_type();
    let i32_ty = ctx.i32_type();
    let i8_ptr = ptr_type!(ctx, i8_type);
    let i32_ptr = ptr_type!(ctx, i32_type);

    // i8* decrypt_strings(i8* src, i32 len, i8* dst)
    let fn_ty = i8_ptr
        .fn_type(&[i8_ptr.into(), i32_ty.into(), i8_ptr.into()], false);

    let decrypt_fn = module.add_function(name, fn_ty, None);
    if inline_fn {
        let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        decrypt_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
    }

    let entry = ctx.append_basic_block(decrypt_fn, "entry");
    let body = ctx.append_basic_block(decrypt_fn, "body");
    let next = ctx.append_basic_block(decrypt_fn, "next");
    let exit = ctx.append_basic_block(decrypt_fn, "exit");

    let builder = ctx.create_builder();

    let src_ptr = decrypt_fn.get_nth_param(0)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get pointer parameter"))?;

    let len = decrypt_fn.get_nth_param(1)
        .map(|param| param.into_int_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get length parameter"))?;

    let dst_ptr = decrypt_fn.get_nth_param(2)
        .map(|param| param.into_pointer_value())
        .ok_or_else(|| anyhow::anyhow!("Failed to get source pointer parameter"))?;

    builder.position_at_end(entry);
    let idx = builder.build_alloca(i32_ty, "idx")?;
    builder.build_store(idx, ctx.i32_type().const_zero())?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(body);
    let index = builder.build_load(i32_ty, idx, "cur_idx")?.into_int_value();
    let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond")?;
    builder.build_conditional_branch(cond, next, exit)?;

    builder.position_at_end(next);
    // 从源地址读取
    let src_gep = unsafe {
        builder.build_gep(i8_ty, src_ptr, &[index], "src_gep")
    }?;
    let ch = builder.build_load(i8_ty, src_gep, "cur")?.into_int_value();
    // 解密
    let xor_ch = i8_ty.const_int(0xAA, false);
    let xored = builder.build_xor(ch, xor_ch, "new")?;
    // 写入目标地址（栈上）
    let dst_gep = unsafe {
        builder.build_gep(i8_ty, dst_ptr, &[index], "dst_gep")
    }?;
    builder.build_store(dst_gep, xored)?;

    let next_index = builder.build_int_add(index, ctx.i32_type().const_int(1, false), "")?;
    builder.build_store(idx, next_index)?;
    builder.build_unconditional_branch(body)?;

    builder.position_at_end(exit);
    // 将目标地址的最后一个字节设置为 \0
    let null_gep = unsafe {
        builder.build_gep(i8_ty, dst_ptr, &[len], "null_gep")
    }?;
    builder.build_store(null_gep, i8_ty.const_zero())?;

    builder.build_return(Some(&dst_ptr))?;

    Ok(decrypt_fn)
}

fn do_global<'a>(module: &mut Module<'a>, gs: &[EncryptedGlobalValue], decrypt_fn: FunctionValue<'a>, ctx: ContextRef<'a>) -> anyhow::Result<()> {
    let i32_ty = ctx.i32_type();
    let i32_ptr = ptr_type!(ctx, i32_type);

    let decrypt_stub_ty = ctx.void_type()
        .fn_type(&[], false);
    let decrypt_stub = module.add_function("decrypt_strings_stub", decrypt_stub_ty, None);
    decrypt_stub.set_linkage(Linkage::Internal);

    let entry = ctx.append_basic_block(decrypt_stub, "entry");
    let builder = ctx.create_builder();

    builder.position_at_end(entry);
    for ev in gs {
        let ptr = ev.global.as_pointer_value();
        let len_val = i32_ty.const_int(ev.len as u64, false);
        let flag_ptr = i32_ptr.const_null();
        builder.build_call(decrypt_fn, &[ptr.into(), len_val.into(), flag_ptr.into()], "")?;
    }

    builder.build_return(None)?;

    let priority = 0; // Default priority
    unsafe {
        let module_ref = module.as_mut_ptr() as *mut std::ffi::c_void;
        let function_ref = decrypt_stub.as_value_ref() as *mut std::ffi::c_void;
        amice_llvm::append_to_global_ctors(module_ref, function_ref, priority);
    }

    Ok(())
}
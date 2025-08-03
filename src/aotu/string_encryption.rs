use ascon_hash::{AsconHash256, Digest, Update};
use llvm_plugin::inkwell::module::{Linkage, Module};
use llvm_plugin::{inkwell, FunctionAnalysisManager, LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, BasicValueEnum, FunctionValue, GlobalValue};
use log::{error, info};

#[cfg(any(
    feature = "llvm15-0",
    feature = "llvm16-0",
))]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.$ty().ptr_type(AddressSpace::default())
    };
}

#[cfg(any(
    feature = "llvm17-0",
    feature = "llvm18-1",
    feature = "llvm19-1",
    feature = "llvm20-1"
))]
macro_rules! ptr_type {
    ($cx:ident, $ty:ident) => {
        $cx.ptr_type(AddressSpace::default())
    };
}

enum StringEncryptionType {
    Xor,
    SimdXor
}

impl StringEncryptionType {
    pub fn do_handle(&self, pass: &StringEncryption,  module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        match self {
            StringEncryptionType::Xor => xor::do_handle(pass, module, manager),
            &StringEncryptionType::SimdXor => simd_xor::do_handle(pass, module, manager),
        }
    }

    /// 等级 0~7, 数字越大可能越安全，但是开销更大！
    /// 如果是负数代表可能并不稳定！
    pub fn level(&self) -> i32 {
        match self {
            StringEncryptionType::Xor => 0,
            StringEncryptionType::SimdXor => -4,
        }
    }
}

pub struct StringEncryption {
    enable: bool,
    decrypt_timing: String,
    encryption_type: StringEncryptionType,
    stack_alloc: bool,
    inline_decrypt: bool,
}

impl LlvmModulePass for StringEncryption {
    fn run_pass<'a>(&self, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        if let Err(e) = self.encryption_type.do_handle(self, module, &manager) {
            error!("(strenc) failed to handle string encryption: {}", e);
        }

        PreservedAnalyses::None
    }
}

impl StringEncryption {
    pub fn new(enable: bool) -> Self {
        let algo = match std::env::var("AMICE_STRING_ALGORITHM")
            .unwrap_or("xor".to_string()).to_lowercase().as_str() {
            "xor" => StringEncryptionType::Xor,
            "xorsimd" | "xor_simd" | "simd_xor" | "simdxor" => StringEncryptionType::SimdXor,
            _ => {
                error!("(strenc) unknown string encryption algorithm, using XOR");
                StringEncryptionType::Xor
            }
        };
        let decrypt_timing = std::env::var("AMICE_STRING_DECRYPT_TIMING")
            .unwrap_or("lazy".to_string());
        let stack_alloc = std::env::var("AMICE_STRING_STACK_ALLOC")
            .map_or(false, |v| v.to_lowercase() == "true");
        let inline_decrypt = std::env::var("AMICE_STRING_INLINE_DECRYPT_FN")
            .map_or(false, |v| v.to_lowercase() == "true");

        StringEncryption {
            enable,
            decrypt_timing,
            encryption_type: algo,
            stack_alloc,
            inline_decrypt
        }
    }
}

struct EncryptedGlobalValue<'a> {
    global: GlobalValue<'a>,
    len: u32,
    flag: Option<GlobalValue<'a>>,
    oneshot: bool,
}

mod simd_xor {
    use anyhow::anyhow;
    use llvm_plugin::inkwell::AddressSpace;
    use llvm_plugin::inkwell::module::{Linkage, Module};
    use llvm_plugin::inkwell::targets::TargetTriple;
    use llvm_plugin::inkwell::values::{AnyValueEnum, ArrayValue, AsValueRef, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, InstructionValue};
    use llvm_plugin::{inkwell, ModuleAnalysisManager};
    use llvm_plugin::inkwell::attributes::{Attribute, AttributeLoc};
    use llvm_plugin::inkwell::context::ContextRef;
    use log::{error, warn};
    use rand::Rng;
    use crate::aotu::string_encryption::{array_as_const_string, EncryptedGlobalValue};
    use crate::aotu::StringEncryption;

    pub(crate) fn do_handle<'a>(pass: &StringEncryption, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        let triple = module.get_triple();
        let triple = triple.as_str().to_str().unwrap_or("unknown");
        if triple.starts_with("riscv") {
            warn!("(strenc) SIMD XOR encryption is unstable on RISC-V targets!");
        }

        let ctx = module.get_context();
        let i32_ty = ctx.i32_type();
        let i8_ty = ctx.i8_type();
        let vector256 = i8_ty.array_type(32);

        let has_flag = pass.decrypt_timing == "lazy";

        let decrypt_fn = add_decrypt_function(
            module,
            &format!("simd_xor_cipher_{}", rand::random::<u32>()),
            has_flag,
            pass.inline_decrypt,
            pass.stack_alloc
        )?;
        let mut key = [0u8; 32];
        rand::rng().fill(&mut key);

        let key_global = module.add_global(vector256, Some(AddressSpace::default()), "");
        let array_values = key
            .map(|c| i8_ty.const_int(c as u64, false))
            .map(|v| unsafe { ArrayValue::new(v.as_value_ref()) });
        key_global.set_initializer(&vector256.const_array(
            &array_values
        ));

        let gs: Vec<EncryptedGlobalValue<'a>> = module.get_globals()
            .filter(|global| !matches!(global.get_linkage(), Linkage::External))
            .filter(|global| {
                global.get_section().map_or(true, |section| {
                    section.to_str().map_or(true, |s| s != "llvm.metadata")
                })
            })
            .filter_map(|global| match global.get_initializer()? {
                BasicValueEnum::ArrayValue(arr) => Some((global, None, arr)),
                BasicValueEnum::StructValue(stru) if stru.count_fields() <= 1 => {
                    match stru.get_field_at_index(0)? {
                        BasicValueEnum::ArrayValue(arr) => Some((global, Some(stru), arr)),
                        _ => None,
                    }
                }
                _ => None,
            })
            .filter(|(_, _, arr)| {
                arr.is_const_string()
            })
            .filter_map(|(global, stru, arr)| {
                let s = array_as_const_string(&arr).and_then(|s| str::from_utf8(s).ok())?;
                let mut encoded_str = vec![0u8; s.len()];
                for (i, c) in s.bytes().enumerate() {
                    encoded_str[i] = c ^ key[i % key.len()];
                }
                let unique_name = global.get_name().to_str()
                    .map_or_else(|_| rand::random::<u32>().to_string(), |s| s.to_string());
                Some((unique_name, global, stru, encoded_str))
            })
            .map(|(unique_name, global, stru, encoded_str)| {
                let flag = if has_flag {
                    let flag = module.add_global(i32_ty, None, &format!("dec_flag_simd_{}", unique_name));
                    flag.set_initializer(&i32_ty.const_int(0, false));
                    flag.set_linkage(Linkage::Internal);
                    Some(flag)
                } else {
                    None
                };

                if let Some(stru) = stru {
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

        match pass.decrypt_timing.as_str() {
            "lazy" => do_lazy(&gs, decrypt_fn, &key_global, ctx, pass.stack_alloc)?,
            "global" => {
                todo!("(strenc) SIMD XOR with `global` timing is not implemented yet");
            },
            _ => return Err(anyhow::anyhow!("(strenc) unknown decrypt timing: {}", pass.decrypt_timing))
        }

        Ok(())
    }

    fn do_lazy(
        gs: &[EncryptedGlobalValue],
        decrypt_fn: FunctionValue<'_>,
        global_key: &GlobalValue,
        ctx: ContextRef,
        stack_alloc: bool,
    ) -> anyhow::Result<()> {
        if stack_alloc {
            todo!("(strenc) SIMD XOR stack allocation is not implemented yet");
        }

        for ev in gs {
            let mut uses = Vec::new();
            let mut use_opt = ev.global.get_first_use();
            while let Some(u) = use_opt {
                use_opt = u.get_next_use();
                uses.push(u);
            }

            for u in uses {
                match u.get_user() {
                    AnyValueEnum::InstructionValue(inst) => insert_decrypt_call(
                        ctx,
                        inst,
                        &ev.global,
                        decrypt_fn,
                        global_key,
                        ev.len,
                        ev.flag,
                    )?,
                    AnyValueEnum::IntValue(value) => {
                        if let Some(inst) = value.as_instruction_value() {
                            insert_decrypt_call(
                                ctx,
                                inst,
                                &ev.global,
                                decrypt_fn,
                                global_key,
                                ev.len,
                                ev.flag,
                            )?
                        } else {
                            error!("(strenc) unexpected IntValue user: {:?}", value);
                        }
                    }
                    AnyValueEnum::PointerValue(gv) => {
                        if let Some(inst) = gv.as_instruction_value() {
                            insert_decrypt_call(
                                ctx,
                                inst,
                                &ev.global,
                                decrypt_fn,
                                global_key,
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

    fn insert_decrypt_call<'a>(
        ctx: ContextRef<'a>,
        inst: InstructionValue<'a>,
        global: &GlobalValue<'a>,
        decrypt_fn: FunctionValue<'a>,
        global_key: &GlobalValue,
        len: u32,
        flag: Option<GlobalValue<'a>>,
    ) -> anyhow::Result<()> {
        let i32_ty = ctx.i32_type();
        let parent_bb = inst.get_parent().expect("inst must be in a block");
        let parent_fn = parent_bb.get_parent().expect("block must have parent fn");

        let builder = ctx.create_builder();
        builder.position_before(&inst);
        let ptr = global.as_pointer_value();
        let dst = global.as_pointer_value();
        let len_val = i32_ty.const_int(len as u64, false);
        let flag_ptr = flag.unwrap().as_pointer_value();
        let key = global_key.as_pointer_value();
        builder.build_call(decrypt_fn, &[ptr.into(), dst.into(), len_val.into(), key.into(), flag_ptr.into()], "", )?;

        Ok(())
    }

    fn add_decrypt_function<'a>(
        module: &mut Module<'a>,
        name: &str,
        has_flag: bool,
        inline_fn: bool,
        stack_alloc: bool
    ) -> anyhow::Result<FunctionValue<'a>> {
        // 密钥总是32字节的！必须是32字节的！
        // void @simd_xor_cipher(i8* src, i8* dst, i32 len, i8* key, i32* flag)
        let ctx = module.get_context();
        let i8_ty = ctx.i8_type();
        let i32_ty = ctx.i32_type();
        let i8_ptr = ptr_type!(ctx, i8_ty);
        let i32_ptr = ptr_type!(ctx, i32_ty);
        let vector_256 = i8_ty.vec_type(32);
        let vector_ptr_type = vector_256.ptr_type(AddressSpace::default());

        let fn_ty = i8_ty
            .fn_type(&[i8_ptr.into(), i8_ptr.into(), i32_ty.into(), i8_ptr.into(), i32_ptr.into()], false);
        let decrypt_fn = module.add_function(name, fn_ty, None);

        if inline_fn {
            let inlinehint_attr = ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
            decrypt_fn.add_attribute(AttributeLoc::Function, inlinehint_attr);
        }

        let entry = ctx.append_basic_block(decrypt_fn, "entry");
        let main_loop = ctx.append_basic_block(decrypt_fn, "main_loop");
        let update_flag = ctx.append_basic_block(decrypt_fn, "update_flag");
        let key_prepare = ctx.append_basic_block(decrypt_fn, "key_prepare");
        let next = ctx.append_basic_block(decrypt_fn, "next");
        let check_rest = ctx.append_basic_block(decrypt_fn, "check_rest");
        let rest = ctx.append_basic_block(decrypt_fn, "rest");
        let exit = ctx.append_basic_block(decrypt_fn, "exit");

        let builder = ctx.create_builder();

        let src_ptr = decrypt_fn.get_nth_param(0)
            .map(|param| param.into_pointer_value())
            .ok_or_else(|| anyhow!("Failed to get source pointer parameter"))?;
        let dst_ptr = decrypt_fn.get_nth_param(1)
            .map(|param| param.into_pointer_value())
            .ok_or_else(|| anyhow!("Failed to get destination pointer parameter"))?;
        let len = decrypt_fn.get_nth_param(2)
            .map(|param| param.into_int_value())
            .ok_or_else(|| anyhow!("Failed to get length parameter"))?;
        let key_ptr = decrypt_fn.get_nth_param(3)
            .map(|param| param.into_pointer_value())
            .ok_or_else(|| anyhow!("Failed to get key pointer parameter"))?;
        let flag_ptr = decrypt_fn.get_nth_param(4)
            .map(|param| param.into_pointer_value())
            .ok_or_else(|| anyhow!("Failed to get flag pointer parameter"))?;

        builder.position_at_end(entry);
        let idx = builder.build_alloca(i32_ty, "idx")?;
        builder.build_store(idx, ctx.i32_type().const_zero())?;

        if has_flag {
            let flag = builder.build_load(i32_ty, flag_ptr, "")?.into_int_value();
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
        let key_load_inst = builder.build_load(vector_256, key_ptr, "key_vec")?;
        let key_vec = key_load_inst.into_vector_value();
        builder.build_unconditional_branch(main_loop)?;

        // 检查是否还有完整的32字节块
        builder.position_at_end(main_loop);
        let index = builder.build_load(i32_ty, idx, "cur_idx")?.into_int_value();
        let tmp = builder.build_int_add(index, ctx.i32_type().const_int(31, false), "tmp")?;
        let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, tmp, len, "cond")?;
        builder.build_conditional_branch(cond, next, check_rest)?;

        builder.position_at_end(next);
        let src_gep = unsafe {
            builder.build_gep(i8_ty, src_ptr, &[index], "src_gep")
        }?;
        let src_load_inst = builder.build_load(vector_256, src_gep, "src_vec")?;
        if let Some(load_inst) = src_load_inst.as_instruction_value() {
            load_inst.set_alignment(1)
                .map_err(|e| anyhow!("Failed to set alignment for load instruction: {}", e))?;
        }
        let src_vec = src_load_inst.into_vector_value();
        let xored_vec = builder.build_xor(src_vec, key_vec, "xored_vec")?;
        let dst_gep = unsafe {
            builder.build_gep(i8_ty, dst_ptr, &[index], "dst_gep")
        }?;
        let store_inst = builder.build_store(dst_gep, xored_vec)?;
        store_inst.set_alignment(1)
            .map_err(|e| anyhow!("Failed to set alignment for store instruction: {}", e))?;

        let next_index = builder.build_int_add(index, ctx.i32_type().const_int(32, false), "next_index")?;
        builder.build_store(idx, next_index)?;
        builder.build_unconditional_branch(main_loop)?;

        builder.position_at_end(check_rest);
        // 查询是否有剩余的字节没有处理
        let index = builder.build_load(i32_ty, idx, "")?.into_int_value();
        let cond = builder.build_int_compare(inkwell::IntPredicate::ULT, index, len, "cond2")?;
        builder.build_conditional_branch(cond, rest, exit)?;

        builder.position_at_end(rest);
        // 处理剩余的字节
        // for(;i<len;i++) output[i] = input[i] ^ key[i % 32];

        let src_gep = unsafe {
            builder.build_gep(i8_ty, src_ptr, &[index], "src_rest_gep")
        }?;
        let ch = builder.build_load(i8_ty, src_gep, "cur")?.into_int_value();
        let key_index = builder.build_int_signed_rem(index, ctx.i32_type().const_int(32, false), "key_index")?;
        let key_gep = unsafe {
            builder.build_gep(i8_ty, key_ptr, &[key_index], "key_rest_gep")
        }?;
        let key_ch = builder.build_load(i8_ty, key_gep, "key_cur")?.into_int_value();
        let xored = builder.build_xor(ch, key_ch, "xored_rest")?;
        let dst_gep = unsafe {
            builder.build_gep(i8_ty, dst_ptr, &[index], "dst_rest_gep")
        }?;
        builder.build_store(dst_gep, xored)?;

        let next_index = builder.build_int_add(index, ctx.i32_type().const_int(1, false), "next_index_rest")?;
        builder.build_store(idx, next_index)?;
        builder.build_unconditional_branch(check_rest)?;

        builder.position_at_end(exit);
        if stack_alloc {
            let null_gep = unsafe {
                builder.build_gep(i8_ty, dst_ptr, &[len], "null_gep")
            }?;
            builder.build_store(null_gep, i8_ty.const_zero())?;
        }
        builder.build_return(Some(&dst_ptr))?;

        Ok(decrypt_fn)
    }
}

mod xor {
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
    use crate::aotu::string_encryption::{array_as_const_string, EncryptedGlobalValue};
    use crate::aotu::StringEncryption;

    pub(crate) fn do_handle<'a>(pass: &StringEncryption, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        let ctx = module.get_context();
        let i32_ty = ctx.i32_type();

        let has_flag = pass.decrypt_timing == "lazy";

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

        match pass.decrypt_timing.as_str() {
            "lazy" => do_lazy(&gs, decrypt_fn, ctx, pass.stack_alloc)?,
            "global" => {
                assert!(!pass.stack_alloc, "(strenc) global decrypt timing is not supported with stack allocation");
                do_global(module, &gs, decrypt_fn, ctx)?
            },
            _ => {
                return Err(anyhow::anyhow!("(strenc) unknown decrypt timing: {}", pass.decrypt_timing));
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
}

pub(crate) fn array_as_const_string<'a>(arr: &'a ArrayValue) -> Option<&'a [u8]> {
    let mut len = 0;
    let ptr = unsafe { inkwell::llvm_sys::core::LLVMGetAsString(arr.as_value_ref(), &mut len) };

    if ptr.is_null() {
        None
    } else {
        unsafe { Some(std::slice::from_raw_parts(ptr.cast(), len)) }
    }
}

fn generate_global_value_hash(
    global: &GlobalValue
) -> String {
    let mut hasher = AsconHash256::new();
    if let Ok(name) = global.get_name().to_str(){
        Update::update(&mut hasher, name.as_bytes());
    } else {
        let rand_str = rand::random::<u32>().to_string();
        Update::update(&mut hasher, rand_str.as_bytes());
    }
    let hash = hasher.finalize();
    hex::encode(hash)
}
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
    XOR
}

impl StringEncryptionType {
    pub fn do_handle(&self, module: &mut Module<'_>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        match self {
            StringEncryptionType::XOR => xor::do_handle(module, manager),
        }
    }
}

pub struct StringEncryption {
    enable: bool,
    encryption_type: StringEncryptionType,
}

impl LlvmModulePass for StringEncryption {
    fn run_pass<'a>(&self, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        if let Err(e) = self.encryption_type.do_handle(module, &manager) {
            error!("(strenc) failed to handle string encryption: {}", e);
        }

        PreservedAnalyses::None
    }
}

impl StringEncryption {
    pub fn new(enable: bool) -> Self {
        StringEncryption {
            enable,
            encryption_type: StringEncryptionType::XOR
        }
    }
}

mod xor {
    use inkwell::module::Module;
    use inkwell::values::FunctionValue;
    use llvm_plugin::inkwell::{AddressSpace, Either};
    use llvm_plugin::{inkwell, FunctionAnalysisManager, ModuleAnalysisManager};
    use llvm_plugin::inkwell::module::Linkage;
    use llvm_plugin::inkwell::values::{AnyValueEnum, BasicValue, BasicValueEnum, BasicValueUse, GlobalValue, InstructionValue};
    use log::{error, info, warn};
    use crate::aotu::string_encryption::array_as_const_string;

    pub(crate) fn do_handle<'a>(module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        let ctx = module.get_context();
        let i32_ty = ctx.i32_type();

        let gs: Vec<(GlobalValue<'a>, u32, GlobalValue<'a>)> = module.get_globals()
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
                let flag = module.add_global(i32_ty, None, &format!("dec_flag_{}", unique_name));
                flag.set_initializer(&i32_ty.const_int(0, false));
                flag.set_linkage(Linkage::Internal);

                if let Some(stru) = stru {
                    // Rust-like strings
                    let new_const = ctx.const_string(&encoded_str, false);
                    stru.set_field_at_index(0, new_const);
                    global.set_initializer(&stru);
                    global.set_constant(false);
                    (global,encoded_str.len() as u32, flag)
                } else {
                    // C-like strings
                    let new_const = ctx.const_string(&encoded_str, false);
                    global.set_initializer(&new_const);
                    global.set_constant(false);
                    (global, encoded_str.len() as u32, flag)
                }
            })
            .collect();

        let decrypt_fn = add_decrypt_function(module, &format!("decrypt_strings_{}", rand::random::<u32>()))?;

        for (global, len, flag) in &gs {
            let mut uses = Vec::new();
            let mut use_opt = global.get_first_use();
            while let Some(u) = use_opt {
                use_opt = u.get_next_use();
                uses.push(u);
            }

            for u in uses {
                let insert_decrypt = |inst: InstructionValue<'_>| -> anyhow::Result<()> {
                    let parent_bb = inst.get_parent().expect("inst must be in a block");
                    let parent_fn = parent_bb.get_parent().expect("block must have parent fn");
                    let builder = ctx.create_builder();

                    builder.position_before(&inst);
                    let ptr = global.as_pointer_value();
                    let len_val = i32_ty.const_int(*len as u64, false);
                    let flag_ptr = flag.as_pointer_value();
                    builder.build_call(decrypt_fn, &[ptr.into(), len_val.into(), flag_ptr.into()], "", )?;

                    Ok(())
                };
                match u.get_user() {
                    AnyValueEnum::InstructionValue(inst) => insert_decrypt(inst)?,
                    AnyValueEnum::IntValue(value) => {
                        if let Some(inst) = value.as_instruction_value() {
                            insert_decrypt(inst)?;
                        } else {
                            error!("(strenc) unexpected IntValue user: {:?}", value);
                        }
                    }
                    AnyValueEnum::PointerValue(gv) => {
                        if let Some(inst) = gv.as_instruction_value() {
                            insert_decrypt(inst)?;
                        } else {
                            error!("(strenc) unexpected GlobalValue user: {:?}", gv);
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

    fn add_decrypt_function<'a>(module: &mut Module<'a>, name: &str) -> anyhow::Result<FunctionValue<'a>> {
        let ctx = module.get_context();
        let i8_ty  = ctx.i8_type();
        let i32_ty = ctx.i32_type();
        let i8_ptr = ptr_type!(ctx, i8_type);
        let i32_ptr = ptr_type!(ctx, i32_type);

        // void decrypt_strings(i8* str, i32 len, i32* flag)
        let fn_ty = ctx.void_type()
            .fn_type(&[i8_ptr.into(), i32_ty.into(), i32_ptr.into()], false);
        let decrypt_fn = module.add_function(name, fn_ty, None);

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

        let flag = builder.build_load(i32_ty, flag_ptr, "flag")?.into_int_value();
        let is_decrypted = builder.build_int_compare(inkwell::IntPredicate::EQ, flag, i32_ty.const_zero(), "is_decrypted")?;
        builder.build_conditional_branch(is_decrypted, entry, exit)?;

        builder.position_at_end(entry);
        builder.build_store(flag_ptr, i32_ty.const_int(1, false))?;

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
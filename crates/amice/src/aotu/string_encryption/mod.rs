mod simd_xor;
mod xor;

use crate::aotu::string_encryption::simd_xor::SimdXorAlgo;
use crate::aotu::string_encryption::xor::XorAlgo;
use crate::config::{Config, StringAlgorithm, StringDecryptTiming, StringEncryptionConfig};
use crate::pass_registry::{AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, LLVMValueRefExt, VerifyResult};
use amice_macro::amice;
use amice_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use amice_plugin::inkwell::module::Module;
use amice_plugin::inkwell::values::{
    AnyValueEnum, ArrayValue, AsValueRef, BasicValue, GlobalValue, InstructionOpcode, InstructionValue, PointerValue,
    StructValue,
};
use amice_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses, inkwell};
use inkwell::llvm_sys::core::LLVMGetAsString;
use std::ptr::NonNull;

/// Stack allocation threshold: strings larger than this will use global timing
/// even when stack allocation is enabled
const STACK_ALLOC_THRESHOLD: u32 = 4096; // 4KB

#[amice(
    priority = 1000,
    name = "StringEncryption",
    flag = AmicePassFlag::PipelineStart | AmicePassFlag::ModuleLevel,
    config = StringEncryptionConfig,
)]
#[derive(Default)]
pub struct StringEncryption {}

impl AmicePass for StringEncryption {
    fn init(&mut self, cfg: &Config, _flag: AmicePassFlag) {
        self.default_config = cfg.string_encryption.clone();

        assert!(
            (self.default_config.timing == StringDecryptTiming::Global && !self.default_config.stack_alloc)
                || self.default_config.timing != StringDecryptTiming::Global,
            "stack alloc is not supported with global decrypt timing: {:?}",
            self.default_config.timing
        );
    }

    fn do_pass(&self, module: &mut Module<'_>) -> anyhow::Result<PreservedAnalyses> {
        if !self.default_config.enable {
            return Ok(PreservedAnalyses::All);
        }

        let mut algo: Box<dyn StringEncryptionAlgo> = match self.default_config.algorithm {
            StringAlgorithm::Xor => Box::new(XorAlgo::default()),
            StringAlgorithm::SimdXor => Box::new(SimdXorAlgo::default()),
        };

        if let Err(err) = algo.initialize(&self.default_config, module) {
            error!("initialize failed: {}", err);
            return Ok(PreservedAnalyses::All);
        }

        if let Err(err) = algo.do_string_encrypt(&self.default_config, module) {
            error!("do_string_encrypt failed: {}", err);
            return Ok(PreservedAnalyses::All);
        }

        for x in module.get_functions() {
            if let VerifyResult::Broken(err) = x.verify_function() {
                error!("function {:?} verify failed: {}", x.get_name(), err);
            }
        }

        Ok(PreservedAnalyses::None)
    }
}

pub(super) trait StringEncryptionAlgo {
    fn initialize(&mut self, cfg: &StringEncryptionConfig, module: &mut Module<'_>) -> anyhow::Result<()>;

    fn do_string_encrypt(&mut self, cfg: &StringEncryptionConfig, module: &mut Module<'_>) -> anyhow::Result<()>;
}

#[derive(Copy, Clone)]
struct EncryptedGlobalValue<'a> {
    global: GlobalValue<'a>,
    str_len: u32,
    flag: Option<GlobalValue<'a>>,
    #[allow(dead_code)]
    oneshot: bool,
    /// Whether this specific string should use stack allocation for decryption
    /// This can be false even when overall stack_alloc is true, for strings > 4KB
    use_stack_alloc: bool,
    users: NonNull<Vec<(InstructionValue<'a>, u32)>>,

    /// Rust Struct String or C++ NTTP String
    pub struct_value: Option<StructValue<'a>>,
    pub field_index: Option<u32>,
}

impl<'a> EncryptedGlobalValue<'a> {
    pub fn new_array_string(
        global: GlobalValue<'a>,
        len: u32,
        flag: Option<GlobalValue<'a>>,
        use_stack_alloc: bool,
        user: Vec<(LLVMValueRef, u32)>,
    ) -> Self {
        let user = Box::new(
            user.iter()
                .map(|(value_ref, op_num)| (value_ref.into_instruction_value(), *op_num))
                .collect::<Vec<_>>(),
        );
        EncryptedGlobalValue {
            global,
            str_len: len,
            flag,
            oneshot: false,
            use_stack_alloc,
            users: NonNull::new(Box::leak(user)).unwrap(),
            struct_value: None,
            field_index: None,
        }
    }

    pub fn new_struct_string(
        global: GlobalValue<'a>,
        len: u32,
        flag: Option<GlobalValue<'a>>,
        use_stack_alloc: bool,
        user: Vec<(LLVMValueRef, u32)>,
        struct_value: Option<StructValue<'a>>,
        field_index: Option<u32>,
    ) -> Self {
        let user = Box::new(
            user.iter()
                .map(|(value_ref, op_num)| (value_ref.into_instruction_value(), *op_num))
                .collect::<Vec<_>>(),
        );
        assert!(struct_value.is_some());
        assert!(field_index.is_some());
        EncryptedGlobalValue {
            global,
            str_len: len,
            flag,
            oneshot: false,
            use_stack_alloc,
            users: NonNull::new(Box::leak(user)).unwrap(),
            struct_value,
            field_index,
        }
    }

    #[allow(dead_code)]
    pub fn push(&self, user: InstructionValue<'a>, op_num: u32) {
        unsafe {
            let _ = &(*self.users.as_ptr()).push((user, op_num));
        }
    }

    pub fn is_array_string(&self) -> bool {
        self.struct_value.is_none() && self.field_index.is_none()
    }

    pub fn is_struct_string(&self) -> bool {
        self.struct_value.is_some() && self.field_index.is_some()
    }

    pub fn user_slice(&self) -> &[(InstructionValue<'a>, u32)] {
        unsafe { (*self.users.as_ptr()).as_slice() }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        unsafe { (*self.users.as_ptr()).len() }
    }

    pub fn free(&self) {
        unsafe {
            let _ = Box::from_raw(self.users.as_ptr());
        }
    }
}

#[allow(dead_code)]
pub(crate) fn array_as_rust_string(arr: &ArrayValue) -> Option<String> {
    let str = array_as_const_string(arr)?;
    String::from_utf8(str.to_vec()).ok()
}

#[inline]
pub(crate) fn array_as_const_string<'a>(arr: &'a ArrayValue) -> Option<&'a [u8]> {
    let mut len = 0;
    if arr.is_null() {
        return None;
    }
    let ptr = unsafe { LLVMGetAsString(arr.as_value_ref() as _, &mut len) };
    if ptr.is_null() {
        None
    } else {
        unsafe { Some(std::slice::from_raw_parts(ptr.cast(), len)) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertPointCollection {
    Direct,
    RequiresGlobalFallback,
}

pub(crate) fn collect_insert_points<'a>(
    string_global: GlobalValue,
    user: AnyValueEnum<'a>,
    output: &mut Vec<(LLVMValueRef, u32)>,
) -> InsertPointCollection {
    // Inkwell classifies an instruction by its result type, so calls returning
    // integers, pointers, arrays, or structs do not necessarily arrive as an
    // InstructionValue. Recover the instruction where possible.
    let target_inst = match user {
        AnyValueEnum::InstructionValue(inst) => Some(inst),
        AnyValueEnum::IntValue(value) => value.as_instruction_value(),
        AnyValueEnum::PointerValue(value) => value.as_instruction_value(),
        AnyValueEnum::ArrayValue(value) => value.as_instruction_value(),
        AnyValueEnum::StructValue(value) => value.as_instruction_value(),
        _ => None,
    };

    let Some(inst) = target_inst else {
        // ConstantExpr, constant aggregates and global initializers have no
        // legal instruction insertion point. Their strings must be decrypted
        // by the global constructor instead of producing an error diagnostic.
        return InsertPointCollection::RequiresGlobalFallback;
    };

    let original_len = output.len();
    for i in 0..inst.get_num_operands() {
        if let Some(op) = inst.get_operand(i)
            && let Some(operand) = op.value()
            && operand.as_value_ref() == string_global.as_value_ref()
        {
            output.push((inst.as_value_ref() as _, i));
        }
    }

    if output.len() == original_len {
        // The instruction references the string through a constant expression
        // or aggregate. Replacing only the final operand would not preserve
        // that expression, so use the safe global fallback for the whole string.
        InsertPointCollection::RequiresGlobalFallback
    } else {
        InsertPointCollection::Direct
    }
}

fn alloc_stack_string<'a>(
    module: &mut Module<'a>,
    string: EncryptedGlobalValue,
    in_entry_block: bool,
    inst: &InstructionValue,
) -> anyhow::Result<PointerValue<'a>> {
    let ctx = module.get_context();
    let i32_ty = ctx.i32_type();
    let i8_ty = ctx.i8_type();
    let string_len = i32_ty.const_int(string.str_len as u64 + 1, false);

    let builder = ctx.create_builder();
    if !in_entry_block {
        // 在非入口块分配，许多LLVM优化pass假设所有 alloca 都在入口块
        // 可能阻止某些优化的进行
        // 寄存器提升等优化可能受影响

        // 如果指令是PHI节点，需要在基本块的第一个非PHI指令位置分配
        let insert_point = if inst.get_opcode() == InstructionOpcode::Phi {
            if let Some(parent_bb) = inst.get_parent() {
                parent_bb.get_first_insertion_pt()
            } else {
                *inst
            }
        } else {
            *inst
        };

        builder.position_before(&insert_point);
        let container = builder.build_array_alloca(i8_ty, string_len, "string_container")?;
        return Ok(container);
    }

    if in_entry_block
        && let Some(parent_block) = inst.get_parent()
        && let Some(parent_function) = parent_block.get_parent()
        && let Some(entry_block) = parent_function.get_entry_block()
        && let Some(terminator) = entry_block.get_terminator()
    {
        builder.position_before(&terminator);
        let container = builder.build_array_alloca(i8_ty, string_len, "string_container")?;
        Ok(container)
    } else {
        // 尝试栈入口块分配栈空间失败！
        Err(anyhow::anyhow!("Failed to allocate stack string"))
    }
}

mod simd_xor;
mod xor;

use crate::config::{Config, StringAlgorithm, StringDecryptTiming};
use crate::pass_registry::{AmicePassLoadable, PassPosition};
use amice_llvm::ir::function::get_basic_block_entry;
use amice_macro::amice;
use ascon_hash::Digest;
use inkwell::llvm_sys::core::LLVMGetAsString;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{
    AnyValueEnum, ArrayValue, AsValueRef, BasicValue, GlobalValue, InstructionValue, PointerValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses, inkwell};
use log::{debug, error};
use std::ptr::NonNull;
use rand::Rng;

/// Stack allocation threshold: strings larger than this will use global timing
/// even when stack allocation is enabled
const STACK_ALLOC_THRESHOLD: u32 = 4096; // 4KB

/// Generate random encryption layers for a string
/// Returns (number_of_layers, algorithms_for_each_layer)
pub(crate) fn generate_encryption_layers(max_layers: u8, preferred_algorithm: StringAlgorithm) -> (u8, Vec<StringAlgorithm>) {
    let mut rng = rand::rng();
    let num_layers = rng.random_range(1..=max_layers);
    
    let mut algorithms = Vec::with_capacity(num_layers as usize);
    
    for _ in 0..num_layers {
        // For the first layer, sometimes use the preferred algorithm
        // For subsequent layers, randomly choose between available algorithms
        let algorithm = if algorithms.is_empty() && rng.random::<bool>() {
            // 50% chance to use preferred algorithm for first layer (changed from gen_bool)
            preferred_algorithm
        } else {
            // Randomly choose algorithm
            match rng.random_range(0..2) {
                0 => StringAlgorithm::Xor,
                1 => StringAlgorithm::SimdXor,
                _ => StringAlgorithm::Xor, // fallback
            }
        };
        algorithms.push(algorithm);
    }
    
    (num_layers, algorithms)
}

#[amice(priority = 1000, name = "StringEncryption", position = PassPosition::PipelineStart)]
#[derive(Default)]
pub struct StringEncryption {
    enable: bool,
    timing: StringDecryptTiming,
    encryption_type: StringAlgorithm,
    stack_alloc: bool,
    inline_decrypt: bool,
    only_dot_string: bool,
    allow_non_entry_stack_alloc: bool,
    max_encryption_layers: u8,
}

impl AmicePassLoadable for StringEncryption {
    fn init(&mut self, cfg: &Config, position: PassPosition) -> bool {
        let decrypt_timing = cfg.string_encryption.timing;
        let stack_alloc = cfg.string_encryption.stack_alloc;
        let inline_decrypt = cfg.string_encryption.inline_decrypt;
        let only_llvm_string = cfg.string_encryption.only_dot_str;

        assert!(
            (decrypt_timing == StringDecryptTiming::Global && !stack_alloc)
                || decrypt_timing != StringDecryptTiming::Global,
            "stack alloc is not supported with global decrypt timing: {:?}",
            decrypt_timing
        );

        self.enable = cfg.string_encryption.enable;
        self.timing = decrypt_timing;
        self.encryption_type = cfg.string_encryption.algorithm;
        self.stack_alloc = stack_alloc;
        self.inline_decrypt = inline_decrypt;
        self.only_dot_string = only_llvm_string;
        self.allow_non_entry_stack_alloc = cfg.string_encryption.allow_non_entry_stack_alloc;
        self.max_encryption_layers = cfg.string_encryption.max_encryption_layers;

        self.enable
    }
}

impl LlvmModulePass for StringEncryption {
    fn run_pass<'a>(&self, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        // Apply multi-layer encryption
        if let Err(e) = self.apply_multi_layer_encryption(module, manager) {
            error!("(strenc) failed to handle multi-layer string encryption: {e}");
        }

        PreservedAnalyses::None
    }
}

impl StringEncryption {
    fn apply_multi_layer_encryption<'a>(&self, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> anyhow::Result<()> {
        // For backward compatibility, if max_encryption_layers is 1, use the original single-layer approach
        if self.max_encryption_layers == 1 {
            return match self.encryption_type {
                StringAlgorithm::Xor => xor::do_handle(self, module, manager),
                StringAlgorithm::SimdXor => simd_xor::do_handle(self, module, manager),
            };
        }

        // Multi-layer encryption approach
        debug!("(strenc) applying multi-layer encryption with max {} layers", self.max_encryption_layers);
        
        // For now, let's implement this step by step
        // Step 1: Apply the first layer using existing logic
        match self.encryption_type {
            StringAlgorithm::Xor => xor::do_handle(self, module, manager),
            StringAlgorithm::SimdXor => simd_xor::do_handle(self, module, manager),
        }?;

        // Step 2: TODO - Apply additional layers if max_encryption_layers > 1
        // This will require more sophisticated implementation

        Ok(())
    }
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
    /// Number of encryption layers applied to this string (1 to max_encryption_layers)
    encryption_layers: u8,
    /// The algorithms used for each layer (in application order)
    layer_algorithms: NonNull<Vec<StringAlgorithm>>,
}

impl<'a> EncryptedGlobalValue<'a> {
    pub fn new(
        global: GlobalValue<'a>,
        len: u32,
        flag: Option<GlobalValue<'a>>,
        use_stack_alloc: bool,
        user: Vec<(LLVMValueRef, u32)>,
        encryption_layers: u8,
        layer_algorithms: Vec<StringAlgorithm>,
    ) -> Self {
        let user = Box::new(
            user.iter()
                .map(|(value_ref, op_num)| unsafe { (InstructionValue::new(*value_ref), *op_num) })
                .collect::<Vec<_>>(),
        );
        let algorithms = Box::new(layer_algorithms);
        EncryptedGlobalValue {
            global,
            str_len: len,
            flag,
            oneshot: false,
            use_stack_alloc,
            users: NonNull::new(Box::leak(user)).unwrap(),
            encryption_layers,
            layer_algorithms: NonNull::new(Box::leak(algorithms)).unwrap(),
        }
    }

    #[allow(dead_code)]
    pub fn push(&self, user: InstructionValue<'a>, op_num: u32) {
        unsafe {
            let _ = &(*self.users.as_ptr()).push((user, op_num));
        }
    }

    pub fn user_slice(&self) -> &[(InstructionValue<'a>, u32)] {
        unsafe { (*self.users.as_ptr()).as_slice() }
    }

    pub fn layer_algorithms(&self) -> &[StringAlgorithm] {
        unsafe { (*self.layer_algorithms.as_ptr()).as_slice() }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        unsafe { (*self.users.as_ptr()).len() }
    }

    pub fn free(&self) {
        unsafe {
            let _ = Box::from_raw(self.users.as_ptr());
            let _ = Box::from_raw(self.layer_algorithms.as_ptr());
        }
    }
}

#[allow(dead_code)]
pub(crate) fn array_as_rust_string(arr: &ArrayValue) -> Option<String> {
    let str = array_as_const_string(arr)?;
    String::from_utf8(str.to_vec()).ok()
}

pub(crate) fn array_as_const_string<'a>(arr: &'a ArrayValue) -> Option<&'a [u8]> {
    let mut len = 0;
    let ptr = unsafe { LLVMGetAsString(arr.as_value_ref(), &mut len) };
    if ptr.is_null() {
        None
    } else {
        unsafe { Some(std::slice::from_raw_parts(ptr.cast(), len)) }
    }
}

pub(crate) fn collect_insert_points<'a>(
    string_global: GlobalValue,
    user: AnyValueEnum<'a>,
    output: &mut Vec<(LLVMValueRef, u32)>,
) -> anyhow::Result<()> {
    use std::collections::HashSet;

    // visited: 按 ValueRef 去重，避免重复与潜在环
    let mut visited = HashSet::new();
    let mut worklist = vec![user.as_value_ref()];

    while let Some(curr_ptr) = worklist.pop() {
        // 如果已访问，继续
        if !visited.insert(curr_ptr) {
            continue;
        }

        // 通过 ValueRef 还原为 AnyValueEnum
        let curr = unsafe { AnyValueEnum::new(curr_ptr) };

        // 如果能解析到“指令”层面，就在该指令上找操作数
        // 否则（常见于 PointerValue/ArrayValue 非 instruction 值），
        // 沿着 use 链继续向上游 user 追溯，直到遇到指令为止
        let mut target_inst: Option<InstructionValue<'a>> = None;

        match curr {
            AnyValueEnum::InstructionValue(inst) => {
                target_inst = Some(inst);
            },
            AnyValueEnum::IntValue(v) => {
                if let Some(inst) = v.as_instruction_value() {
                    target_inst = Some(inst);
                } else {
                    error!("(strenc) unexpected IntValue user: {v:?}");
                }
            },
            AnyValueEnum::PointerValue(v) => {
                if let Some(inst) = v.as_instruction_value() {
                    target_inst = Some(inst);
                } else {
                    let mut found = false;
                    let mut use_opt = v.get_first_use();
                    while let Some(u) = use_opt {
                        use_opt = u.get_next_use();
                        found = true;
                        debug!("{:?}", u.get_user());
                        worklist.push(u.get_user().as_value_ref());
                    }
                    if !found {
                        error!("(strenc) unexpected PointerValue user (no uses): {v:?}");
                    }
                }
            },
            AnyValueEnum::ArrayValue(v) => {
                let mut found = false;
                let mut use_opt = v.get_first_use();
                while let Some(u) = use_opt {
                    use_opt = u.get_next_use();
                    found = true;
                    worklist.push(u.get_user().as_value_ref());
                }
                if !found {
                    error!("(strenc) unexpected ArrayValue user (no uses): {v:?}");
                }
            },
            // 其他类型：目前未覆盖，打印日志
            _ => error!("(strenc) unexpected user type: {curr:?}"),
        }

        // 在找到的目标指令上遍历其操作数，定位引用到目标全局的操作数索引
        if let Some(inst) = target_inst {
            for i in 0..inst.get_num_operands() {
                if let Some(op) = inst.get_operand(i) {
                    if let Some(operand) = op.left() {
                        // 只收集直接引用的插入点
                        if operand.as_value_ref() == string_global.as_value_ref() {
                            output.push((inst.as_value_ref(), i));
                        }
                    }
                }
            }
        }
    }

    Ok(())
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
        builder.position_before(inst);
        let container = builder.build_array_alloca(i8_ty, string_len, "string_container")?;
        return Ok(container);
    }

    if in_entry_block
        && let Some(parent_block) = inst.get_parent()
        && let Some(parent_function) = parent_block.get_parent()
        && let Some(entry_block) = get_basic_block_entry(parent_function)
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

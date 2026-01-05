mod simd_xor;
mod xor;

use crate::aotu::string_encryption::simd_xor::SimdXorAlgo;
use crate::aotu::string_encryption::xor::XorAlgo;
use crate::config::{Config, StringAlgorithm, StringDecryptTiming, StringEncryptionConfig};
use crate::pass_registry::{AmicePass, AmicePassFlag};
use amice_llvm::inkwell2::{BasicBlockExt, FunctionExt, LLVMValueRefExt, VerifyResult};
use amice_macro::amice;
use inkwell::llvm_sys::core::LLVMGetAsString;
use llvm_plugin::inkwell::llvm_sys::prelude::LLVMValueRef;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{
    AnyValueEnum, ArrayValue, AsValueRef, BasicValue, GlobalValue, InstructionOpcode, InstructionValue, PointerValue,
};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses, inkwell};
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
}

impl<'a> EncryptedGlobalValue<'a> {
    pub fn new(
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
            AnyValueEnum::StructValue(v) => {
                // %3 = call { ptr, i64 } @_ZN3fmt3v1112vformat_to_nIPcJETnNSt9enable_ifIXsr6detail18is_output_iteratorIT_cEE5valueEiE4typeELi0EEENS0_18format_to_n_resultIS4_EES4_mNS0_17basic_string_viewIcEENS0_17basic_format_argsINS0_7contextEEE(ptr noundef nonnull %2, i64 noundef 1023, ptr nonnull @.str, i64 2, i64 12, ptr nonnull %1)
                // { ptr, i64 } 是一个匿名结构体类型，包含两个字段：
                // 调用这个函数返回匿名结构体，很明显这个是一条指令，但是没有走InstructionValue！为什么呢？
                // 因为inkwell的安全封装设计，解决办法旧很简单了，直接复用inst的逻辑，llvm兜底
                // ---- 作为指令：它是一条 call 指令（Instruction）
                // ---- 作为值：它产生一个 { ptr, i64 } 类型的值（Value）
                if let Some(inst) = v.as_instruction_value() {
                    target_inst = Some(inst);
                } else {
                    error!("(strenc) unexpected StructValue user: {v:?}");
                }
            },
            // 其他类型：目前未覆盖，打印日志
            _ => error!("(strenc) unexpected user type: {curr:?}"),
        }

        // 在找到的目标指令上遍历其操作数，定位引用到目标全局的操作数索引
        if let Some(inst) = target_inst {
            for i in 0..inst.get_num_operands() {
                if let Some(op) = inst.get_operand(i) {
                    if let Some(operand) = op.value() {
                        // 只收集直接引用的插入点
                        if operand.as_value_ref() == string_global.as_value_ref() {
                            output.push((inst.as_value_ref() as _, i));
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

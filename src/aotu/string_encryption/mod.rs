mod simd_xor;
mod xor;

use crate::config::{CONFIG, StringAlgorithm, StringDecryptTiming};
use ascon_hash::{AsconHash256, Digest, Update};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, GlobalValue};
use llvm_plugin::{LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses, inkwell};
use log::error;

/// Stack allocation threshold: strings larger than this will use global timing
/// even when stack allocation is enabled
const STACK_ALLOC_THRESHOLD: u32 = 4096; // 4KB

enum StringEncryptionType {
    Xor,
    SimdXor,
}

impl StringEncryptionType {
    pub fn do_handle(
        &self,
        pass: &StringEncryption,
        module: &mut Module<'_>,
        manager: &ModuleAnalysisManager,
    ) -> anyhow::Result<()> {
        match self {
            StringEncryptionType::Xor => xor::do_handle(pass, module, manager),
            StringEncryptionType::SimdXor => simd_xor::do_handle(pass, module, manager),
        }
    }

    /// 等级 0~7, 数字越大可能越安全，但是开销更大！
    /// 如果是负数代表可能并不稳定！
    pub fn level(&self) -> i32 {
        match self {
            StringEncryptionType::Xor => 0,
            StringEncryptionType::SimdXor => 4,
        }
    }
}

pub struct StringEncryption {
    enable: bool,
    decrypt_timing: StringDecryptTiming,
    encryption_type: StringEncryptionType,
    stack_alloc: bool,
    inline_decrypt: bool,
    only_llvm_string: bool,
}

impl LlvmModulePass for StringEncryption {
    fn run_pass<'a>(&self, module: &mut Module<'a>, manager: &ModuleAnalysisManager) -> PreservedAnalyses {
        if !self.enable {
            return PreservedAnalyses::All;
        }

        if let Err(e) = self.encryption_type.do_handle(self, module, manager) {
            error!("(strenc) failed to handle string encryption: {e}");
        }

        PreservedAnalyses::None
    }
}

impl StringEncryption {
    pub fn new(enable: bool) -> Self {
        let cfg = &*CONFIG;
        let algo = match cfg.string_encryption.algorithm {
            StringAlgorithm::Xor => StringEncryptionType::Xor,
            StringAlgorithm::SimdXor => StringEncryptionType::SimdXor,
        };
        let decrypt_timing = cfg.string_encryption.decrypt_timing;
        let stack_alloc = cfg.string_encryption.stack_alloc;
        let inline_decrypt = cfg.string_encryption.inline_decrypt_fn;
        let only_llvm_string = cfg.string_encryption.only_llvm_string;

        assert!(
            (decrypt_timing == StringDecryptTiming::Global && !stack_alloc)
                || decrypt_timing != StringDecryptTiming::Global,
            "stack alloc is not supported with global decrypt timing: {:?}",
            decrypt_timing
        );

        StringEncryption {
            enable,
            decrypt_timing,
            encryption_type: algo,
            stack_alloc,
            inline_decrypt,
            only_llvm_string,
        }
    }
}

struct EncryptedGlobalValue<'a> {
    global: GlobalValue<'a>,
    len: u32,
    flag: Option<GlobalValue<'a>>,
    oneshot: bool,
    /// Whether this specific string should use stack allocation for decryption
    /// This can be false even when overall stack_alloc is true, for strings > 4KB
    use_stack_alloc: bool,
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

pub(crate) fn generate_global_value_hash(global: &GlobalValue) -> String {
    let mut hasher = AsconHash256::new();
    if let Ok(name) = global.get_name().to_str() {
        Update::update(&mut hasher, name.as_bytes());
    } else {
        let rand_str = rand::random::<u32>().to_string();
        Update::update(&mut hasher, rand_str.as_bytes());
    }
    let hash = hasher.finalize();
    hex::encode(hash)
}

mod xor;
mod simd_xor;

use ascon_hash::{AsconHash256, Digest, Update};
use llvm_plugin::inkwell::module::{Module};
use llvm_plugin::{inkwell, LlvmModulePass, ModuleAnalysisManager, PreservedAnalyses};
use llvm_plugin::inkwell::values::{ArrayValue, AsValueRef, GlobalValue};
use log::{error};

enum StringEncryptionType {
    Xor,
    SimdXor
}

#[derive(Debug, Clone)]
enum DecryptTiming {
    Lazy,
    Global,
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
    decrypt_timing: DecryptTiming,
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
        let decrypt_timing = match std::env::var("AMICE_STRING_DECRYPT_TIMING")
            .unwrap_or("lazy".to_string()).to_lowercase().as_str() {
            "lazy" => DecryptTiming::Lazy,
            "global" => DecryptTiming::Global,
            _ => {
                error!("(strenc) unknown decrypt timing, using lazy");
                DecryptTiming::Lazy
            }
        };
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

pub(crate) fn array_as_const_string<'a>(arr: &'a ArrayValue) -> Option<&'a [u8]> {
    let mut len = 0;
    let ptr = unsafe { inkwell::llvm_sys::core::LLVMGetAsString(arr.as_value_ref(), &mut len) };

    if ptr.is_null() {
        None
    } else {
        unsafe { Some(std::slice::from_raw_parts(ptr.cast(), len)) }
    }
}

pub(crate) fn generate_global_value_hash(
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
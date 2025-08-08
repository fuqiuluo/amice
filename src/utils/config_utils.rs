use lazy_static::lazy_static;
use bitflags::bitflags;
use log::{error, warn};

#[derive(Debug, Clone)]
pub struct Config {
    pub string_encryption: StringEncryptionConfig,
    pub indirect_call: IndirectCallConfig,
    pub indirect_branch: IndirectBranchConfig,
    pub split_basic_block: SplitBasicBlockConfig,
    pub vm_flatten: VmFlattenConfig,
}

#[derive(Debug, Clone)]
pub struct StringEncryptionConfig {
    pub enable: bool, // 是否启用字符串加密
    pub algorithm: StringAlgorithm,         // 控制字符串的加密算法 xor, simd_xor
    pub decrypt_timing: StringDecryptTiming,    // 控制字符串的解密时机 lazy, global
    pub stack_alloc: bool,         // 控制解密字符串的是否分配到栈内存 true/false
    pub inline_decrypt_fn: bool,   // 控制是否内联解密函数 true/false
    pub only_llvm_string: bool,    // 控制是否只加密`.str`字符串 true/false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringAlgorithm {
    Xor, // 使用异或加密字符串
    SimdXor, // (beta) 使用SIMD指令的异或加密字符串
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringDecryptTiming {
    Lazy,
    Global,
}

#[derive(Debug, Clone)]
pub struct IndirectCallConfig {
    pub enable: bool, // 是否启用间接跳转
    pub xor_key: Option<u32>, // 间接跳转下标xor密钥  `0`关闭间接跳转下标加密
}

#[derive(Debug, Clone)]
pub struct IndirectBranchConfig {
    pub enable: bool, // 是否启用间接指令
    pub flags: IndirectBranchFlags, // 间接指令的额外混淆扩展功能，以逗号分隔的字符串形式指定
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IndirectBranchFlags: u32 {
        const Basic =             0b00000001;
        const DummyBlock =        0b00000010;
        const ChainedDummyBlock = 0b00000110; // note: includes DummyBlock
        const EncryptBlockIndex = 0b00001000;
    }
}

#[derive(Debug, Clone)]
pub struct SplitBasicBlockConfig {
    pub enable: bool, // 是否启用切割基本块
    pub num: u32, // 切割基本块次数 3
}

#[derive(Debug, Clone)]
pub struct VmFlattenConfig {
    pub enable: bool, // 是否启用虚拟机扁平化
}

fn is_truthy(value: &str) -> bool {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "on" => true,
        "0" | "false" | "off" => false,
        _ => {
            warn!("Unknown boolean value: \"{value}\", defaulting to false");
            false
        },
    }
}

fn bool_var(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => is_truthy(&v),
        Err(_) => default,
    }
}

fn string_var(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn u32_var(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

fn optional_u32_var(key: &str) -> Option<u32> {
    std::env::var(key).ok().and_then(|v| v.parse::<u32>().ok())
}

fn parse_string_algorithm(value: &str) -> StringAlgorithm {
    match value.to_lowercase().as_str() {
        "xor" => StringAlgorithm::Xor,
        "xorsimd" | "xor_simd" | "simd_xor" | "simdxor" => StringAlgorithm::SimdXor,
        _ => {
            error!("(strenc) unknown string encryption algorithm, using XOR");
            StringAlgorithm::Xor
        }
    }
}

fn parse_string_decrypt_timing(value: &str) -> StringDecryptTiming {
    match value.to_lowercase().as_str() {
        "lazy" => StringDecryptTiming::Lazy,
        "global" => StringDecryptTiming::Global,
        _ => {
            error!("(strenc) unknown decrypt timing, using lazy");
            StringDecryptTiming::Lazy
        }
    }
}

fn parse_indirect_branch_flags(value: &str) -> IndirectBranchFlags {
    let mut flags = IndirectBranchFlags::empty();
    for x in value.split(',') {
        let x = x.trim().to_lowercase();
        if x.is_empty() { continue; }
        match x.as_str() {
            "dummy_block" => flags |= IndirectBranchFlags::DummyBlock,
            "chained_dummy_blocks" => flags |= IndirectBranchFlags::ChainedDummyBlock,
            "encrypt_block_index" => flags |= IndirectBranchFlags::EncryptBlockIndex,
            _ => warn!("Unknown AMICE_INDIRECT_BRANCH_FLAGS: \"{x}\", ignoring"),
        }
    }
    flags
}

lazy_static! {
    pub static ref CONFIG: Config = {
        let string_encryption = StringEncryptionConfig {
            enable: bool_var("AMICE_STRING_ENCRYPTION", true),
            algorithm: parse_string_algorithm(&string_var("AMICE_STRING_ALGORITHM", "xor")),
            decrypt_timing: parse_string_decrypt_timing(&string_var("AMICE_STRING_DECRYPT_TIMING", "lazy")),
            stack_alloc: bool_var("AMICE_STRING_STACK_ALLOC", false),
            inline_decrypt_fn: bool_var("AMICE_STRING_INLINE_DECRYPT_FN", false),
            only_llvm_string: bool_var("AMICE_STRING_ONLY_LLVM_STRING", true),
        };

        let indirect_call = IndirectCallConfig {
            enable: bool_var("AMICE_INDIRECT_CALL", true),
            xor_key: optional_u32_var("AMICE_INDIRECT_CALL_XOR_KEY"),
        };

        let indirect_branch = IndirectBranchConfig {
            enable: bool_var("AMICE_INDIRECT_BRANCH", true),
            flags: parse_indirect_branch_flags(&string_var("AMICE_INDIRECT_BRANCH_FLAGS", "")),
        };

        let split_basic_block = SplitBasicBlockConfig {
            enable: bool_var("AMICE_SPLIT_BASIC_BLOCK", false),
            num: u32_var("AMICE_SPLIT_BASIC_BLOCK_NUM", 3),
        };

        let vm_flatten = VmFlattenConfig {
            enable: bool_var("AMICE_VM_FLATTEN", false),
        };

        Config {
            string_encryption,
            indirect_call,
            indirect_branch,
            split_basic_block,
            vm_flatten,
        }
    };
}




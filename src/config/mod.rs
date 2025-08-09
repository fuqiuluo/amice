use bitflags::bitflags;
use lazy_static::lazy_static;
use log::{error, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub string_encryption: StringEncryptionConfig,
    pub indirect_call: IndirectCallConfig,
    pub indirect_branch: IndirectBranchConfig,
    pub split_basic_block: SplitBasicBlockConfig,
    pub vm_flatten: VmFlattenConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StringEncryptionConfig {
    pub enable: bool,                        // 是否启用字符串加密
    pub algorithm: StringAlgorithm,          // 控制字符串的加密算法 xor, simd_xor
    pub decrypt_timing: StringDecryptTiming, // 控制字符串的解密时机 lazy, global
    pub stack_alloc: bool,                   // 控制解密字符串的是否分配到栈内存 true/false
    pub inline_decrypt_fn: bool,             // 控制是否内联解密函数 true/false
    pub only_llvm_string: bool,              // 控制是否只加密`.str`字符串 true/false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringAlgorithm {
    Xor,     // 使用异或加密字符串
    SimdXor, // (beta) 使用SIMD指令的异或加密字符串
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StringDecryptTiming {
    Lazy,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectCallConfig {
    pub enable: bool,         // 是否启用间接跳转
    pub xor_key: Option<u32>, // 间接跳转下标xor密钥  `0`关闭间接跳转下标加密
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IndirectBranchFlags: u32 {
        const Basic =             0b00000001;
        const DummyBlock =        0b00000010;
        const ChainedDummyBlock = 0b00000110; // note: includes DummyBlock
        const EncryptBlockIndex = 0b00001000;
        const DummyJunk =         0b00010000; // 在 dummy block 中插入干扰性指令
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectBranchConfig {
    pub enable: bool, // 是否启用间接指令
    #[serde(deserialize_with = "deserialize_indirect_branch_flags")]
    pub flags: IndirectBranchFlags, // 支持数值位掩码、CSV 字符串或字符串数组
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitBasicBlockConfig {
    pub enable: bool, // 是否启用切割基本块
    pub num: u32,     // 切割基本块次数 3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

fn parse_string_algorithm(value: &str) -> StringAlgorithm {
    match value.to_lowercase().as_str() {
        "xor" => StringAlgorithm::Xor,
        "xorsimd" | "xor_simd" | "simd_xor" | "simdxor" => StringAlgorithm::SimdXor,
        _ => {
            error!("(strenc) unknown string encryption algorithm, using XOR");
            StringAlgorithm::Xor
        },
    }
}

fn parse_string_decrypt_timing(value: &str) -> StringDecryptTiming {
    match value.to_lowercase().as_str() {
        "lazy" => StringDecryptTiming::Lazy,
        "global" => StringDecryptTiming::Global,
        _ => {
            error!("(strenc) unknown decrypt timing, using lazy");
            StringDecryptTiming::Lazy
        },
    }
}

fn parse_indirect_branch_flags(value: &str) -> IndirectBranchFlags {
    let mut flags = IndirectBranchFlags::empty();
    for x in value.split(',') {
        let x = x.trim().to_lowercase();
        if x.is_empty() {
            continue;
        }
        match x.as_str() {
            "dummy_block" => flags |= IndirectBranchFlags::DummyBlock,
            "chained_dummy_blocks" => flags |= IndirectBranchFlags::ChainedDummyBlock,
            "encrypt_block_index" => flags |= IndirectBranchFlags::EncryptBlockIndex,
            "dummy_junk" => flags |= IndirectBranchFlags::DummyJunk,
            _ => warn!("Unknown AMICE_INDIRECT_BRANCH_FLAGS: \"{x}\", ignoring"),
        }
    }
    flags
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IndirectBranchFlagsRepr {
    Bits(u32),
    One(String),
    Many(Vec<String>),
}

fn deserialize_indirect_branch_flags<'de, D>(deserializer: D) -> Result<IndirectBranchFlags, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let repr = IndirectBranchFlagsRepr::deserialize(deserializer)?;
    let flags = match repr {
        IndirectBranchFlagsRepr::Bits(bits) => IndirectBranchFlags::from_bits_truncate(bits),
        IndirectBranchFlagsRepr::One(s) => parse_indirect_branch_flags(&s),
        IndirectBranchFlagsRepr::Many(arr) => {
            let mut all = IndirectBranchFlags::empty();
            for s in arr {
                all |= parse_indirect_branch_flags(&s);
            }
            all
        },
    };
    Ok(flags)
}

lazy_static! {
    pub static ref CONFIG: Config = {
        let mut cfg = load_from_file_env().unwrap_or_default();
        overlay_env(&mut cfg);
        cfg
    };
}

fn load_from_file_env() -> Option<Config> {
    let path = std::env::var("AMICE_CONFIG_PATH").ok()?;
    load_from_file(Path::new(&path)).ok()
}

fn load_from_file(path: &Path) -> anyhow::Result<Config> {
    let content = fs::read_to_string(path)?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let mut cfg: Config = match ext.as_str() {
        "toml" => toml::from_str(&content)?,
        "yml" | "yaml" => serde_yaml::from_str(&content)?,
        "json" => serde_json::from_str(&content)?,
        _ => {
            if let Ok(v) = toml::from_str(&content) {
                v
            } else if let Ok(v) = serde_yaml::from_str(&content) {
                v
            } else {
                serde_json::from_str(&content)?
            }
        },
    };
    overlay_env(&mut cfg);
    Ok(cfg)
}

fn overlay_env(cfg: &mut Config) {
    // String enc
    if std::env::var("AMICE_STRING_ENCRYPTION").is_ok() {
        cfg.string_encryption.enable = bool_var("AMICE_STRING_ENCRYPTION", cfg.string_encryption.enable);
    }
    if let Ok(v) = std::env::var("AMICE_STRING_ALGORITHM") {
        cfg.string_encryption.algorithm = parse_string_algorithm(&v);
    }
    if let Ok(v) = std::env::var("AMICE_STRING_DECRYPT_TIMING") {
        cfg.string_encryption.decrypt_timing = parse_string_decrypt_timing(&v);
    }
    if std::env::var("AMICE_STRING_STACK_ALLOC").is_ok() {
        cfg.string_encryption.stack_alloc = bool_var("AMICE_STRING_STACK_ALLOC", cfg.string_encryption.stack_alloc);
    }
    if std::env::var("AMICE_STRING_INLINE_DECRYPT_FN").is_ok() {
        cfg.string_encryption.inline_decrypt_fn = bool_var(
            "AMICE_STRING_INLINE_DECRYPT_FN",
            cfg.string_encryption.inline_decrypt_fn,
        );
    }
    if std::env::var("AMICE_STRING_ONLY_LLVM_STRING").is_ok() {
        cfg.string_encryption.only_llvm_string =
            bool_var("AMICE_STRING_ONLY_LLVM_STRING", cfg.string_encryption.only_llvm_string);
    }

    // Indirect call
    if std::env::var("AMICE_INDIRECT_CALL").is_ok() {
        cfg.indirect_call.enable = bool_var("AMICE_INDIRECT_CALL", cfg.indirect_call.enable);
    }
    if let Ok(v) = std::env::var("AMICE_INDIRECT_CALL_XOR_KEY") {
        cfg.indirect_call.xor_key = v.parse::<u32>().ok();
    }

    // Indirect branch
    if std::env::var("AMICE_INDIRECT_BRANCH").is_ok() {
        cfg.indirect_branch.enable = bool_var("AMICE_INDIRECT_BRANCH", cfg.indirect_branch.enable);
    }
    if let Ok(v) = std::env::var("AMICE_INDIRECT_BRANCH_FLAGS") {
        cfg.indirect_branch.flags |= parse_indirect_branch_flags(&v);
    }

    // Split basic block
    if std::env::var("AMICE_SPLIT_BASIC_BLOCK").is_ok() {
        cfg.split_basic_block.enable = bool_var("AMICE_SPLIT_BASIC_BLOCK", cfg.split_basic_block.enable);
    }
    if let Ok(v) = std::env::var("AMICE_SPLIT_BASIC_BLOCK_NUM") {
        cfg.split_basic_block.num = v.parse::<u32>().unwrap_or(cfg.split_basic_block.num);
    }

    // Vm flatten
    if std::env::var("AMICE_VM_FLATTEN").is_ok() {
        cfg.vm_flatten.enable = bool_var("AMICE_VM_FLATTEN", cfg.vm_flatten.enable);
    }
}

impl Default for StringEncryptionConfig {
    fn default() -> Self {
        Self {
            enable: true,
            algorithm: StringAlgorithm::Xor,
            decrypt_timing: StringDecryptTiming::Lazy,
            stack_alloc: false,
            inline_decrypt_fn: false,
            only_llvm_string: true,
        }
    }
}

impl Default for IndirectCallConfig {
    fn default() -> Self {
        Self {
            enable: true,
            xor_key: None,
        }
    }
}

impl Default for IndirectBranchConfig {
    fn default() -> Self {
        Self {
            enable: true,
            flags: IndirectBranchFlags::empty(),
        }
    }
}

impl Default for SplitBasicBlockConfig {
    fn default() -> Self {
        Self { enable: false, num: 3 }
    }
}

impl Default for VmFlattenConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

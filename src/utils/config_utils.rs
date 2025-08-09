use lazy_static::lazy_static;
use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use log::{error, warn};

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
    pub enable: bool, // 是否启用字符串加密
    pub algorithm: StringAlgorithm,         // 控制字符串的加密算法 xor, simd_xor
    pub decrypt_timing: StringDecryptTiming,    // 控制字符串的解密时机 lazy, global
    pub stack_alloc: bool,         // 控制解密字符串的是否分配到栈内存 true/false
    pub inline_decrypt_fn: bool,   // 控制是否内联解密函数 true/false
    pub only_llvm_string: bool,    // 控制是否只加密`.str`字符串 true/false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringAlgorithm {
    Xor, // 使用异或加密字符串
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
    pub enable: bool, // 是否启用间接跳转
    pub xor_key: Option<u32>, // 间接跳转下标xor密钥  `0`关闭间接跳转下标加密
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectBranchConfig {
    pub enable: bool, // 是否启用间接指令
    #[serde(deserialize_with = "deserialize_indirect_branch_flags")]
    pub flags: IndirectBranchFlags, // 支持数值位掩码、CSV 字符串或字符串数组
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IndirectBranchFlags: u32 {
        const Basic =             0b00000001;
        const DummyBlock =        0b00000010;
        const ChainedDummyBlock = 0b00000110; // note: includes DummyBlock
        const EncryptBlockIndex = 0b00001000;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitBasicBlockConfig {
    pub enable: bool, // 是否启用切割基本块
    pub num: u32, // 切割基本块次数 3
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
        }
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
            // try formats by best effort
            if let Ok(v) = toml::from_str(&content) { v }
            else if let Ok(v) = serde_yaml::from_str(&content) { v }
            else { serde_json::from_str(&content)? }
        }
    };
    // Overlay env overrides (env has highest priority)
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
        cfg.string_encryption.inline_decrypt_fn = bool_var("AMICE_STRING_INLINE_DECRYPT_FN", cfg.string_encryption.inline_decrypt_fn);
    }
    if std::env::var("AMICE_STRING_ONLY_LLVM_STRING").is_ok() {
        cfg.string_encryption.only_llvm_string = bool_var("AMICE_STRING_ONLY_LLVM_STRING", cfg.string_encryption.only_llvm_string);
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
        Self { enable: true, xor_key: None }
    }
}

impl Default for IndirectBranchConfig {
    fn default() -> Self {
        Self { enable: true, flags: IndirectBranchFlags::empty() }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn tmp_file(ext: &str, content: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("amice_config_test_{}.{ext}", rand::random::<u64>()));
        let mut file = fs::File::create(&path).expect("create tmp config file");
        file.write_all(content.as_bytes()).expect("write tmp config");
        path
    }

    static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    fn guard_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_GUARD.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn clear_env() {
        unsafe {
            std::env::remove_var("AMICE_CONFIG_PATH");
            std::env::remove_var("AMICE_STRING_ENCRYPTION");
            std::env::remove_var("AMICE_STRING_ALGORITHM");
            std::env::remove_var("AMICE_STRING_DECRYPT_TIMING");
            std::env::remove_var("AMICE_STRING_STACK_ALLOC");
            std::env::remove_var("AMICE_STRING_INLINE_DECRYPT_FN");
            std::env::remove_var("AMICE_STRING_ONLY_LLVM_STRING");
            std::env::remove_var("AMICE_INDIRECT_CALL");
            std::env::remove_var("AMICE_INDIRECT_CALL_XOR_KEY");
            std::env::remove_var("AMICE_INDIRECT_BRANCH");
            std::env::remove_var("AMICE_INDIRECT_BRANCH_FLAGS");
            std::env::remove_var("AMICE_SPLIT_BASIC_BLOCK");
            std::env::remove_var("AMICE_SPLIT_BASIC_BLOCK_NUM");
            std::env::remove_var("AMICE_VM_FLATTEN");
        }
    }

    fn build_cfg_from_path(path: Option<&Path>) -> Config {
        let mut cfg = match path {
            Some(p) => load_from_file(p).unwrap_or_default(),
            None => Config::default(),
        };
        overlay_env(&mut cfg);
        cfg
    }

    #[test]
    fn env_only_truthy_and_defaults() {
        let _g = guard_env();
        clear_env();
        let cfg = build_cfg_from_path(None);
        assert_eq!(cfg.string_encryption.enable, true);
        assert_eq!(cfg.string_encryption.algorithm, StringAlgorithm::Xor);
        assert_eq!(cfg.string_encryption.decrypt_timing, StringDecryptTiming::Lazy);
        assert_eq!(cfg.string_encryption.stack_alloc, false);
        assert_eq!(cfg.string_encryption.inline_decrypt_fn, false);
        assert_eq!(cfg.string_encryption.only_llvm_string, true);

        assert_eq!(cfg.indirect_call.enable, true);
        assert!(cfg.indirect_call.xor_key.is_none());

        assert_eq!(cfg.indirect_branch.enable, true);

        assert_eq!(cfg.split_basic_block.enable, false);
        assert_eq!(cfg.split_basic_block.num, 3);

        assert_eq!(cfg.vm_flatten.enable, false);
    }

    #[test]
    fn file_toml_parsing_and_env_overlay() {
        let _g = guard_env();
        clear_env();
        let toml = r#"
[string_encryption]
enable = false
algorithm = "simd_xor"
decrypt_timing = "global"
stack_alloc = true
inline_decrypt_fn = true
only_llvm_string = false

[indirect_call]
enable = false
xor_key = 123

[indirect_branch]
enable = true
flags = ["dummy_block", "chained_dummy_blocks"]

[split_basic_block]
enable = true
num = 7

[vm_flatten]
enable = true
"#;
        let path = tmp_file("toml", toml);
        unsafe { std::env::set_var("AMICE_CONFIG_PATH", &path); }

        // Overlay via env
        unsafe { std::env::set_var("AMICE_STRING_ENCRYPTION", "on"); }
        unsafe { std::env::set_var("AMICE_INDIRECT_BRANCH_FLAGS", "encrypt_block_index"); }

        let cfg = build_cfg_from_path(Some(&path));
        assert_eq!(cfg.string_encryption.enable, true); // env overlay
        assert_eq!(cfg.string_encryption.algorithm, StringAlgorithm::SimdXor);
        assert_eq!(cfg.string_encryption.decrypt_timing, StringDecryptTiming::Global);
        assert_eq!(cfg.string_encryption.stack_alloc, true);
        assert_eq!(cfg.string_encryption.inline_decrypt_fn, true);
        assert_eq!(cfg.string_encryption.only_llvm_string, false);

        assert_eq!(cfg.indirect_call.enable, false);
        assert_eq!(cfg.indirect_call.xor_key, Some(123));

        assert!(cfg.indirect_branch.flags.contains(IndirectBranchFlags::DummyBlock));
        assert!(cfg.indirect_branch.flags.contains(IndirectBranchFlags::ChainedDummyBlock));
        // overlay adds encrypt flag
        assert!(cfg.indirect_branch.flags.contains(IndirectBranchFlags::EncryptBlockIndex));

        assert_eq!(cfg.split_basic_block.enable, true);
        assert_eq!(cfg.split_basic_block.num, 7);

        assert_eq!(cfg.vm_flatten.enable, true);
    }

    #[test]
    fn file_yaml_and_json_parsing() {
        let _g = guard_env();
        clear_env();
        let yaml = r#"
string_encryption:
  enable: true
  algorithm: xor
  decrypt_timing: lazy
  stack_alloc: false
  inline_decrypt_fn: false
  only_llvm_string: true
indirect_call:
  enable: true
  xor_key: 0
indirect_branch:
  enable: true
  flags: "dummy_block,chained_dummy_blocks"
split_basic_block:
  enable: false
  num: 3
vm_flatten:
  enable: false
"#;
        let yaml_path = tmp_file("yaml", yaml);

        unsafe { std::env::set_var("AMICE_CONFIG_PATH", &yaml_path); }
        let cfg_yaml = build_cfg_from_path(Some(&yaml_path));
        assert!(cfg_yaml.indirect_branch.flags.contains(IndirectBranchFlags::DummyBlock));
        assert!(cfg_yaml.indirect_branch.flags.contains(IndirectBranchFlags::ChainedDummyBlock));

        // JSON
        clear_env();
        let json = r#"
{
  "string_encryption": {
    "enable": true,
    "algorithm": "xor",
    "decrypt_timing": "lazy",
    "stack_alloc": false,
    "inline_decrypt_fn": false,
    "only_llvm_string": true
  },
  "indirect_call": {
    "enable": true,
    "xor_key": 0
  },
  "indirect_branch": {
    "enable": true,
    "flags": ["dummy_block", "encrypt_block_index"]
  },
  "split_basic_block": {
    "enable": false,
    "num": 3
  },
  "vm_flatten": {
    "enable": false
  }
}
"#;
        let json_path = tmp_file("json", json);
        unsafe { std::env::set_var("AMICE_CONFIG_PATH", &json_path); }
        let cfg_json = build_cfg_from_path(Some(&json_path));
        assert!(cfg_json.indirect_branch.flags.contains(IndirectBranchFlags::DummyBlock));
        assert!(cfg_json.indirect_branch.flags.contains(IndirectBranchFlags::EncryptBlockIndex));
    }
}
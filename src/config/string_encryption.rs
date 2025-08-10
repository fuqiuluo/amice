use super::{EnvOverlay, bool_var};
use log::error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StringEncryptionConfig {
    /// 是否启用字符串加密
    #[serde(alias = "enable")]
    pub enable: bool,

    /// 加密/解密算法
    pub algorithm: StringAlgorithm,

    /// 解密时机
    pub timing: StringDecryptTiming,

    /// 是否开启栈栈上解密
    pub stack_alloc: bool,

    /// 是否将解密函数标记为 inline
    #[serde(alias = "inline_decrypt_fn")]
    pub inline_decrypt: bool,

    /// 仅处理 `.str` 段中的字符串
    pub only_dot_str: bool,

    /// 是否允许在非入口块也进行栈上解密临时分配
    /// false 时将把相关栈分配限制在入口块，便于优化与栈生存期分析
    pub allow_non_entry_stack_alloc: bool,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringAlgorithm {
    Xor,
    SimdXor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StringDecryptTiming {
    Lazy,
    Global,
}

impl Default for StringEncryptionConfig {
    fn default() -> Self {
        Self {
            enable: true,
            algorithm: StringAlgorithm::Xor,
            timing: StringDecryptTiming::Lazy,
            stack_alloc: false,
            inline_decrypt: false,
            only_dot_str: true,
            allow_non_entry_stack_alloc: false,
        }
    }
}

pub(crate) fn parse_string_algorithm(value: &str) -> StringAlgorithm {
    match value.to_lowercase().as_str() {
        "xor" => StringAlgorithm::Xor,
        "xorsimd" | "xor_simd" | "simd_xor" | "simdxor" => StringAlgorithm::SimdXor,
        _ => {
            error!("(strenc) unknown string encryption algorithm, using XOR");
            StringAlgorithm::Xor
        },
    }
}

pub(crate) fn parse_string_decrypt_timing(value: &str) -> StringDecryptTiming {
    match value.to_lowercase().as_str() {
        "lazy" => StringDecryptTiming::Lazy,
        "global" => StringDecryptTiming::Global,
        _ => {
            error!("(strenc) unknown decrypt timing, using Global");
            StringDecryptTiming::Global
        },
    }
}

impl EnvOverlay for StringEncryptionConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_STRING_ENCRYPTION").is_ok() {
            self.enable = bool_var("AMICE_STRING_ENCRYPTION", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_STRING_ALGORITHM") {
            self.algorithm = parse_string_algorithm(&v);
        }
        if let Ok(v) = std::env::var("AMICE_STRING_DECRYPT_TIMING") {
            self.timing = parse_string_decrypt_timing(&v);
        }
        if std::env::var("AMICE_STRING_STACK_ALLOC").is_ok() {
            self.stack_alloc = bool_var("AMICE_STRING_STACK_ALLOC", self.stack_alloc);
        }
        if std::env::var("AMICE_STRING_INLINE_DECRYPT_FN").is_ok() {
            self.inline_decrypt = bool_var("AMICE_STRING_INLINE_DECRYPT_FN", self.inline_decrypt);
        }
        if std::env::var("AMICE_STRING_ONLY_LLVM_STRING").is_ok() {
            self.only_dot_str = bool_var("AMICE_STRING_ONLY_LLVM_STRING", self.only_dot_str);
        }
        if std::env::var("AMICE_STRING_ONLY_DOT_STRING").is_ok() {
            self.only_dot_str = bool_var("AMICE_STRING_ONLY_DOT_STRING", self.only_dot_str);
        }
        if std::env::var("AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC").is_ok() {
            self.allow_non_entry_stack_alloc = bool_var("AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC", self.allow_non_entry_stack_alloc);
        }
    }
}

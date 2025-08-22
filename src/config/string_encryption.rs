use super::{EnvOverlay, bool_var};
use log::error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StringEncryptionConfig {
    /// Whether to enable string encryption obfuscation
    #[serde(alias = "enable")]
    pub enable: bool,

    /// Encryption/decryption algorithm to use
    pub algorithm: StringAlgorithm,

    /// When to decrypt strings during execution
    pub timing: StringDecryptTiming,

    /// Whether to enable stack-based decryption
    pub stack_alloc: bool,

    /// Whether to mark decryption functions as inline
    #[serde(alias = "inline_decrypt_fn")]
    pub inline_decrypt: bool,

    /// Only process strings from the `.str` section
    pub only_dot_str: bool,

    /// Allow stack allocation for decryption in non-entry blocks
    /// When false, limits stack allocations to entry blocks for better optimization
    pub allow_non_entry_stack_alloc: bool,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringAlgorithm {
    #[default]
    Xor,
    SimdXor,
}

impl StringAlgorithm {
    /// Security level 0-7: higher numbers may be more secure but with greater overhead
    /// Negative numbers indicate potentially unstable implementations
    #[allow(dead_code)]
    pub fn level(&self) -> i32 {
        match self {
            StringAlgorithm::Xor => 0,
            StringAlgorithm::SimdXor => 4,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StringDecryptTiming {
    Lazy,
    #[default]
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
            self.allow_non_entry_stack_alloc = bool_var(
                "AMICE_STRING_ALLOW_NON_ENTRY_STACK_ALLOC",
                self.allow_non_entry_stack_alloc,
            );
        }
    }
}

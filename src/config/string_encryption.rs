use super::{EnvOverlay, bool_var};
use log::error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StringEncryptionConfig {
    pub enable: bool,
    pub algorithm: StringAlgorithm,
    pub decrypt_timing: StringDecryptTiming,
    pub stack_alloc: bool,
    pub inline_decrypt_fn: bool,
    pub only_llvm_string: bool,
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
            decrypt_timing: StringDecryptTiming::Lazy,
            stack_alloc: false,
            inline_decrypt_fn: false,
            only_llvm_string: true,
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
            error!("(strenc) unknown decrypt timing, using lazy");
            StringDecryptTiming::Lazy
        },
    }
}

impl EnvOverlay for StringEncryptionConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_STRING_ENCRYPTION").is_ok() {
            self.enable = bool_var("AMICE_STRING_ENCRYPTION", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_STRING_ALGORITHM") {
            self.algorithm = super::string_encryption::parse_string_algorithm(&v);
        }
        if let Ok(v) = std::env::var("AMICE_STRING_DECRYPT_TIMING") {
            self.decrypt_timing = super::string_encryption::parse_string_decrypt_timing(&v);
        }
        if std::env::var("AMICE_STRING_STACK_ALLOC").is_ok() {
            self.stack_alloc = bool_var("AMICE_STRING_STACK_ALLOC", self.stack_alloc);
        }
        if std::env::var("AMICE_STRING_INLINE_DECRYPT_FN").is_ok() {
            self.inline_decrypt_fn = bool_var("AMICE_STRING_INLINE_DECRYPT_FN", self.inline_decrypt_fn);
        }
        if std::env::var("AMICE_STRING_ONLY_LLVM_STRING").is_ok() {
            self.only_llvm_string = bool_var("AMICE_STRING_ONLY_LLVM_STRING", self.only_llvm_string);
        }
    }
}

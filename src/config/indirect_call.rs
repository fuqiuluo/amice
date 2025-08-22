use super::{EnvOverlay, bool_var};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectCallConfig {
    /// Whether to enable indirect call obfuscation
    pub enable: bool,
    /// Optional XOR key for encrypting function pointers (None for random key)
    pub xor_key: Option<u32>,
}

impl Default for IndirectCallConfig {
    fn default() -> Self {
        Self {
            enable: true,
            xor_key: None,
        }
    }
}

impl EnvOverlay for IndirectCallConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_INDIRECT_CALL").is_ok() {
            self.enable = bool_var("AMICE_INDIRECT_CALL", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_INDIRECT_CALL_XOR_KEY") {
            self.xor_key = v.parse::<u32>().ok();
        }
    }
}

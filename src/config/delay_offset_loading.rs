use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelayOffsetLoadingConfig {
    pub enable: bool,
    pub xor_offset: bool,
}

impl Default for DelayOffsetLoadingConfig {
    fn default() -> Self {
        Self {
            enable: false,
            xor_offset: true,
        }
    }
}

impl EnvOverlay for DelayOffsetLoadingConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_DELAY_OFFSET_LOADING").is_ok() {
            self.enable = bool_var("AMICE_DELAY_OFFSET_LOADING", self.enable);
        }

        if std::env::var("AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET").is_ok() {
            self.xor_offset = bool_var("AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET", self.xor_offset);
        }
    }
}

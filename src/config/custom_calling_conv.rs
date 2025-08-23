use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};
use crate::config::bool_var;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomCallingConvConfig {
    pub enable: bool,
}

impl Default for CustomCallingConvConfig {
    fn default() -> Self {
        Self {
            enable: true
        }
    }
}

impl EnvOverlay for CustomCallingConvConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_CUSTOM_CALLING_CONV").is_ok() {
            self.enable = bool_var("AMICE_CUSTOM_CALLING_CONV", self.enable);
        }
    }
}

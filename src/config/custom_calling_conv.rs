use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

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
    fn overlay_env(&mut self) {}
}

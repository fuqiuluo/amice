use serde::{Deserialize, Serialize};
use crate::config::{bool_var};
use crate::pass_registry::EnvOverlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelayOffsetLoadingConfig {
    pub enable: bool,
}

impl Default for DelayOffsetLoadingConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for DelayOffsetLoadingConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_DELAY_OFFSET_LOADING").is_ok() {
            self.enable = bool_var("AMICE_DELAY_OFFSET_LOADING", self.enable);
        }
    }
}
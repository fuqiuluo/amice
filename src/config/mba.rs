use serde::{Deserialize, Serialize};
use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MbaConfig {
    pub enable: bool,
}

impl Default for MbaConfig {
    fn default() -> Self {
        Self {
            enable: false,
        }
    }
}

impl EnvOverlay for MbaConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_MBA").is_ok() {
            self.enable = bool_var("AMICE_MBA", self.enable);
        }
    }
}
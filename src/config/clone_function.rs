use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CloneFunctionConfig {
    pub enable: bool,
}

impl Default for CloneFunctionConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for CloneFunctionConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_CLONE_FUNCTION").is_ok() {
            self.enable = bool_var("AMICE_CLONE_FUNCTION", self.enable);
        }
    }
}

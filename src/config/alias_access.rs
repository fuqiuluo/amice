use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AliasAccessConfig {
    pub enable: bool,
}

impl Default for AliasAccessConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for AliasAccessConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_ALIAS_ACCESS").is_ok() {
            self.enable = bool_var("AMICE_ALIAS_ACCESS", self.enable);
        }
    }
}

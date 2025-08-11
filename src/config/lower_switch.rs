use serde::{Deserialize, Serialize};
use crate::config::{bool_var, IndirectCallConfig};
use crate::pass_registry::EnvOverlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct LowerSwitchConfig {
    pub enable: bool,
}

impl EnvOverlay for LowerSwitchConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_LOWER_SWITCH").is_ok() {
            self.enable = bool_var("AMICE_LOWER_SWITCH", self.enable);
        }
    }
}

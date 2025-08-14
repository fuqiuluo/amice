use super::bool_var;
use crate::pass_registry::EnvOverlay;
use bitflags::bitflags;
use log::warn;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BogusControlFlowConfig {
    pub enable: bool,
}

impl Default for BogusControlFlowConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for BogusControlFlowConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_BOGUS_CONTROL_FLOW").is_ok() {
            self.enable = bool_var("AMICE_BOGUS_CONTROL_FLOW", self.enable);
        }
    }
}

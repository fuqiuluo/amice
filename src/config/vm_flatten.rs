use super::{EnvOverlay, bool_var};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VmFlattenConfig {
    /// Whether to enable virtual machine based control flow flattening
    pub enable: bool,
}

impl Default for VmFlattenConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for VmFlattenConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_VM_FLATTEN").is_ok() {
            self.enable = bool_var("AMICE_VM_FLATTEN", self.enable);
        }
    }
}

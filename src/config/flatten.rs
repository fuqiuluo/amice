use crate::config::bool_var;
use crate::config::indirect_branch::parse_indirect_branch_flags;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlattenConfig {
    pub enable: bool,
    pub fix_stack: bool,
}

impl Default for FlattenConfig {
    fn default() -> Self {
        Self {
            enable: false,
            fix_stack: true,
        }
    }
}

impl EnvOverlay for FlattenConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_FLATTEN").is_ok() {
            self.enable = bool_var("AMICE_FLATTEN", self.enable);
        }

        if std::env::var("AMICE_FLATTEN_FIX_STACK").is_ok() {
            self.fix_stack = bool_var("AMICE_FLATTEN_FIX_STACK", self.fix_stack);
        }
    }
}

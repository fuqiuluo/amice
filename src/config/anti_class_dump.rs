use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AntiClassDumpConfig {
    pub enable: bool,
    pub use_initialize: bool,
}

impl EnvOverlay for AntiClassDumpConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_ANTI_CLASS_DUMP").is_ok() {
            self.enable = bool_var("AMICE_ANTI_CLASS_DUMP", self.enable);
        }

        if std::env::var("AMICE_ANTI_CLASS_DUMP_USE_INITIALIZE").is_ok() {
            self.use_initialize = bool_var("AMICE_ANTI_CLASS_DUMP_USE_INITIALIZE", self.use_initialize);
        }
    }
}

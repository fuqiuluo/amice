use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AliasAccessConfig {
    pub enable: bool,
    pub shuffle_raw_box: bool,
    pub loose_raw_box: bool,
}

impl EnvOverlay for AliasAccessConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_ALIAS_ACCESS").is_ok() {
            self.enable = bool_var("AMICE_ALIAS_ACCESS", self.enable);
        }

        if std::env::var("AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX").is_ok() {
            self.shuffle_raw_box = bool_var("AMICE_ALIAS_ACCESS_SHUFFLE_RAW_BOX", self.shuffle_raw_box);
        }

        if std::env::var("AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX").is_ok() {
            self.loose_raw_box = bool_var("AMICE_ALIAS_ACCESS_LOOSE_RAW_BOX", self.loose_raw_box);
        }
    }
}

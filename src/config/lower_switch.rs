use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct LowerSwitchConfig {
    pub enable: bool,
    pub append_dummy_code: bool,
}

impl EnvOverlay for LowerSwitchConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_LOWER_SWITCH").is_ok() {
            self.enable = bool_var("AMICE_LOWER_SWITCH", self.enable);
        }

        if std::env::var("AMICE_LOWER_SWITCH_WITH_DUMMY_CODE").is_ok() {
            self.append_dummy_code = bool_var("AMICE_LOWER_SWITCH_WITH_DUMMY_CODE", self.append_dummy_code);
        }
    }
}

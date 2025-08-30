use serde::{Deserialize, Serialize};
use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamAggregateConfig {
    pub enable: bool,
}

impl Default for ParamAggregateConfig {
    fn default() -> Self {
        Self {
            enable: false,
        }
    }
}

impl EnvOverlay for ParamAggregateConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_PARAM_AGGREGATE").is_ok() {
            self.enable = bool_var("AMICE_PARAM_AGGREGATE", self.enable);
        }
    }
}

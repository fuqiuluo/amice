use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeExtractorConfig {
    pub enable: bool,
}

impl Default for CodeExtractorConfig {
    fn default() -> Self {
        Self { enable: false }
    }
}

impl EnvOverlay for CodeExtractorConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_CODE_EXTRACTOR").is_ok() {
            self.enable = bool_var("AMICE_CODE_EXTRACTOR", self.enable);
        }
    }
}

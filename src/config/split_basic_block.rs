use serde::{Deserialize, Serialize};
use super::{EnvOverlay, bool_var};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitBasicBlockConfig {
    pub enable: bool,
    pub num: u32,
}

impl Default for SplitBasicBlockConfig {
    fn default() -> Self {
        Self { enable: false, num: 3 }
    }
}

impl EnvOverlay for SplitBasicBlockConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_SPLIT_BASIC_BLOCK").is_ok() {
            self.enable = bool_var("AMICE_SPLIT_BASIC_BLOCK", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_SPLIT_BASIC_BLOCK_NUM") {
            self.num = v.parse::<u32>().unwrap_or(self.num);
        }
    }
}


use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct BasicBlockOutliningConfig {
    pub enable: bool,
    pub max_extractor_size: usize,
}

impl EnvOverlay for BasicBlockOutliningConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_BASIC_BLOCK_OUTLINING").is_ok() {
            self.enable = bool_var("AMICE_BASIC_BLOCK_OUTLINING", self.enable);
        }

        if std::env::var("AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE").is_ok() {
            self.max_extractor_size = usize::from_str_radix(
                &std::env::var("AMICE_BASIC_BLOCK_OUTLINING_MAX_EXTRACTOR_SIZE").unwrap(),
                10,
            )
            .unwrap();
        } else {
            self.max_extractor_size = 16;
        }
    }
}

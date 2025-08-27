use crate::config::bool_var;
use crate::pass_registry::EnvOverlay;
use log::error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AliasAccessConfig {
    pub enable: bool,
    pub mode: AliasAccessMode,

    /// Shuffle the RawBox order
    ///
    /// Parameters available only in `PointerChain` mode
    pub shuffle_raw_box: bool,
    /// Loose RawBox, the gap will fill the garbage
    ///
    /// Parameters available only in `PointerChain` mode
    pub loose_raw_box: bool,
}

#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize)]
pub enum AliasAccessMode {
    #[serde(rename = "pointer_chain")]
    #[default]
    PointerChain,
}

fn parse_alias_access_mode(s: &str) -> Result<AliasAccessMode, String> {
    let s = s.to_lowercase();
    match s.as_str() {
        "pointer_chain" | "basic" | "v1" => Ok(AliasAccessMode::PointerChain),
        _ => Err(format!("unknown alias access mode: {}", s)),
    }
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

        if let Ok(mode) = std::env::var("AMICE_ALIAS_ACCESS_MODE") {
            self.mode = parse_alias_access_mode(&mode).unwrap_or_else(|e| {
                error!("parse alias access mode failed: {}", e);
                AliasAccessMode::default()
            })
        }
    }
}

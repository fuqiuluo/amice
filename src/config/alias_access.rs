use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use crate::config::bool_var;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use log::error;
use serde::{Deserialize, Serialize};
use amice_llvm::inkwell2::ModuleExt;
use crate::config::eloquent_config::EloquentConfigParser;

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

impl FunctionAnnotationsOverlay for AliasAccessConfig {
    type Config = AliasAccessConfig;

    fn overlay_annotations<'a>(&self, module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<Self> {
        let mut cfg = self.clone();
        let annotations_expr = module.read_function_annotate(function)
            .map_err(|e| anyhow::anyhow!("read function annotations failed: {}", e))?
            .join(" ");

        let mut parser = EloquentConfigParser::new();
        parser.parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser
            .get_bool("alias_access")
            .or_else(|| parser.get_bool("aliasaccess"))
            .or_else(|| parser.get_bool("alias")) // 兼容Polaris-Obfuscator
            .map(|v| cfg.enable = v);
        parser.get_string("alias_access_mode").map(|v| cfg.mode = parse_alias_access_mode(&v).unwrap_or_else(|e| {
            error!("parse alias access mode failed: {}", e);
            AliasAccessMode::default()
        }));
        parser.get_bool("alias_access_shuffle_raw_box").map(|v| cfg.shuffle_raw_box = v);
        parser.get_bool("alias_access_loose_raw_box").map(|v| cfg.loose_raw_box = v);

        Ok(cfg)
    }
}
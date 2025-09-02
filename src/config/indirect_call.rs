use super::{EnvOverlay, bool_var};
use crate::pass_registry::FunctionAnnotationsOverlay;
use amice_llvm::inkwell2::ModuleExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectCallConfig {
    /// Whether to enable indirect call obfuscation
    pub enable: bool,
    /// Optional XOR key for encrypting function pointers (None for random key)
    pub xor_key: Option<u32>,
}

impl Default for IndirectCallConfig {
    fn default() -> Self {
        Self {
            enable: false,
            xor_key: None,
        }
    }
}

impl EnvOverlay for IndirectCallConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_INDIRECT_CALL").is_ok() {
            self.enable = bool_var("AMICE_INDIRECT_CALL", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_INDIRECT_CALL_XOR_KEY") {
            self.xor_key = v.parse::<u32>().ok();
        }
    }
}

impl FunctionAnnotationsOverlay for IndirectCallConfig {
    type Config = Self;

    fn overlay_annotations<'a>(
        &self,
        module: &mut llvm_plugin::inkwell::module::Module<'a>,
        function: llvm_plugin::inkwell::values::FunctionValue<'a>,
    ) -> anyhow::Result<Self::Config> {
        let mut cfg = self.clone();
        let annotations_expr = module
            .read_function_annotate(function)
            .map_err(|e| anyhow::anyhow!("read function annotations failed: {}", e))?
            .join(" ");

        let mut parser = crate::config::eloquent_config::EloquentConfigParser::new();
        parser
            .parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser
            .get_bool("indirect_call")
            .or_else(|| parser.get_bool("icall"))
            .or_else(|| parser.get_bool("indirectcall"))
            .map(|v| cfg.enable = v);

        Ok(cfg)
    }
}

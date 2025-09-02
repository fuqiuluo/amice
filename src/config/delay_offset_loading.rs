use crate::config::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelayOffsetLoadingConfig {
    pub enable: bool,
    pub xor_offset: bool,
}

impl Default for DelayOffsetLoadingConfig {
    fn default() -> Self {
        Self {
            enable: false,
            xor_offset: true,
        }
    }
}

impl EnvOverlay for DelayOffsetLoadingConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_DELAY_OFFSET_LOADING").is_ok() {
            self.enable = bool_var("AMICE_DELAY_OFFSET_LOADING", self.enable);
        }

        if std::env::var("AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET").is_ok() {
            self.xor_offset = bool_var("AMICE_DELAY_OFFSET_LOADING_XOR_OFFSET", self.xor_offset);
        }
    }
}

impl FunctionAnnotationsOverlay for DelayOffsetLoadingConfig {
    type Config = DelayOffsetLoadingConfig;

    fn overlay_annotations<'a>(
        &self,
        module: &mut Module<'a>,
        function: FunctionValue<'a>,
    ) -> anyhow::Result<Self::Config> {
        let mut cfg = self.clone();
        let annotations_expr = module
            .read_function_annotate(function)
            .map_err(|e| anyhow::anyhow!("read function annotations failed: {}", e))?
            .join(" ");

        let mut parser = EloquentConfigParser::new();
        parser
            .parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser
            .get_bool("delay_offset_loading")
            .or_else(|| parser.get_bool("ama"))
            .or_else(|| parser.get_bool("delay_offset_loading_xor_offset"))
            .map(|v| cfg.enable = v);


        parser
            .get_bool("delay_offset_loading_xor_offset")
            .or_else(|| parser.get_bool("ama_xor_offset"))
            .or_else(|| parser.get_bool("ama_xor"))
            .map(|v| cfg.xor_offset = v);

        Ok(cfg)
    }
}

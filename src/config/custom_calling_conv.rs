use crate::config::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomCallingConvConfig {
    pub enable: bool,
}

impl Default for CustomCallingConvConfig {
    fn default() -> Self {
        Self { enable: true }
    }
}

impl EnvOverlay for CustomCallingConvConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_CUSTOM_CALLING_CONV").is_ok() {
            self.enable = bool_var("AMICE_CUSTOM_CALLING_CONV", self.enable);
        }
    }
}

impl FunctionAnnotationsOverlay for CustomCallingConvConfig {
    type Config = CustomCallingConvConfig;

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
            .get_bool("custom_calling_conv")
            .or_else(|| parser.get_bool("custom_cc"))
            .map(|v| cfg.enable = v);

        Ok(cfg)
    }
}

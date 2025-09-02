use super::{EnvOverlay, bool_var};
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::FunctionAnnotationsOverlay;
use amice_llvm::inkwell2::ModuleExt;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FunctionWrapperConfig {
    /// Whether to enable function wrapper obfuscation
    pub enable: bool,
    /// Probability percentage for each call site to be obfuscated (0-100)
    pub probability: u32,
    /// Number of times to apply the wrapper transformation per call site
    pub times: u32,
}

impl Default for FunctionWrapperConfig {
    fn default() -> Self {
        Self {
            enable: false,
            probability: 70,
            times: 3,
        }
    }
}

impl EnvOverlay for FunctionWrapperConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_FUNCTION_WRAPPER").is_ok() {
            self.enable = bool_var("AMICE_FUNCTION_WRAPPER", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_FUNCTION_WRAPPER_PROBABILITY") {
            if let Ok(prob) = v.parse::<u32>() {
                self.probability = prob.min(100);
            }
        }
        if let Ok(v) = std::env::var("AMICE_FUNCTION_WRAPPER_TIMES") {
            if let Ok(times) = v.parse::<u32>() {
                self.times = times.max(1);
            }
        }
    }
}

impl FunctionAnnotationsOverlay for FunctionWrapperConfig {
    type Config = Self;

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
            .get_bool("function_wrapper")
            .or_else(|| parser.get_bool("func_wrapper"))
            .map(|v| cfg.enable = v);

        parser
            .get_number::<u32>("function_wrapper_probability")
            .or_else(|| parser.get_number::<u32>("func_wrapper_probability"))
            .or_else(|| parser.get_number::<u32>("function_wrapper_prob"))
            .or_else(|| parser.get_number::<u32>("func_wrapper_prob"))
            .map(|v| cfg.probability = v.min(100));

        Ok(cfg)
    }
}

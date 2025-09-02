use crate::config::bool_var;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct LowerSwitchConfig {
    /// Whether to enable switch statement lowering to if-else chains
    pub enable: bool,
    /// Append dummy code to obfuscate the lowered if-else structure
    pub append_dummy_code: bool,
}

impl EnvOverlay for LowerSwitchConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_LOWER_SWITCH").is_ok() {
            self.enable = bool_var("AMICE_LOWER_SWITCH", self.enable);
        }

        if std::env::var("AMICE_LOWER_SWITCH_WITH_DUMMY_CODE").is_ok() {
            self.append_dummy_code = bool_var("AMICE_LOWER_SWITCH_WITH_DUMMY_CODE", self.append_dummy_code);
        }
    }
}

impl FunctionAnnotationsOverlay for LowerSwitchConfig {
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
            .get_bool("lower_switch")
            .or_else(|| parser.get_bool("lowerswitch"))
            .or_else(|| parser.get_bool("switch_to_if"))
            .map(|v| cfg.enable = v);

        parser
            .get_bool("lower_switch_with_dummy_code")
            .or_else(|| parser.get_bool("lowerswitchwithdummycode"))
            .or_else(|| parser.get_bool("switch_to_if_with_dummy_code"))
            .map(|v| cfg.append_dummy_code = v);

        Ok(cfg)
    }
}

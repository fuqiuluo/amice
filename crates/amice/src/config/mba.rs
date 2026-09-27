use crate::config::{bool_var, eloquent_config::EloquentConfigParser};
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use amice_plugin::inkwell::{module::Module, values::FunctionValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MbaConfig {
    pub enable: bool,
    /// Allow exact binary64 mantissa regions on supported hardware FP targets.
    pub float_regions: bool,
    /// Expand integer identities directly into the FP region graph.
    pub pre_expand: bool,
    /// Read a private global once per invocation to choose the FP carrier.
    pub opaque_guard: bool,
    /// Original instructions admitted per function (hard limit: 512).
    pub max_instructions: u32,
    /// Conservative bound on emitted instructions, including edge conversions.
    pub max_added_instructions: u32,
}

impl Default for MbaConfig {
    fn default() -> Self {
        Self {
            enable: false,
            float_regions: true,
            pre_expand: true,
            opaque_guard: true,
            max_instructions: 128,
            max_added_instructions: 2048,
        }
    }
}

impl EnvOverlay for MbaConfig {
    fn overlay_env(&mut self) {
        self.enable = bool_var("AMICE_MBA", self.enable);
        self.float_regions = bool_var("AMICE_MBA_FLOAT_REGIONS", self.float_regions);
        self.pre_expand = bool_var("AMICE_MBA_PRE_EXPAND", self.pre_expand);
        self.opaque_guard = bool_var("AMICE_MBA_OPAQUE_GUARD", self.opaque_guard);
        if let Ok(v) = std::env::var("AMICE_MBA_MAX_INSTRUCTIONS") {
            self.max_instructions = v.parse().unwrap_or(self.max_instructions);
        }
        if let Ok(v) = std::env::var("AMICE_MBA_MAX_ADDED_INSTRUCTIONS") {
            self.max_added_instructions = v.parse().unwrap_or(self.max_added_instructions);
        }
    }
}

impl FunctionAnnotationsOverlay for MbaConfig {
    type Config = MbaConfig;

    fn overlay_annotations<'a>(&self, module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<Self> {
        let mut cfg = self.clone();
        let annotations = module
            .read_function_annotate(function)
            .map_err(|e| anyhow::anyhow!("read function annotations failed: {e}"))?
            .join(" ");
        let mut parser = EloquentConfigParser::new();
        parser
            .parse(&annotations)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {e}"))?;
        if let Some(v) = parser.get_bool("mba") {
            cfg.enable = v;
        }
        if let Some(v) = parser.get_bool("mba_float_regions") {
            cfg.float_regions = v;
        }
        if let Some(v) = parser.get_bool("mba_pre_expand") {
            cfg.pre_expand = v;
        }
        if let Some(v) = parser.get_bool("mba_opaque_guard") {
            cfg.opaque_guard = v;
        }
        if let Some(v) = parser.get_number("mba_max_instructions") {
            cfg.max_instructions = v;
        }
        if let Some(v) = parser.get_number("mba_max_added_instructions") {
            cfg.max_added_instructions = v;
        }
        Ok(cfg)
    }
}

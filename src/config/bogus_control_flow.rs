use super::bool_var;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use log::warn;
use serde::{Deserialize, Serialize};
use std::cmp::{max, min};
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use amice_llvm::inkwell2::ModuleExt;
use crate::config::eloquent_config::EloquentConfigParser;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BogusControlFlowConfig {
    pub enable: bool,
    pub mode: BogusControlFlowMode,
    /// Probability (0-100) that a basic block will be obfuscated
    pub probability: u32,
    /// Number of times to run the obfuscation pass on the function
    pub loop_count: u32,
}

#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize)]
pub enum BogusControlFlowMode {
    #[serde(rename = "basic")]
    #[default]
    Basic,
    #[serde(rename = "polaris-primes")]
    PolarisPrimes,
}

fn parse_alias_access_mode(s: &str) -> Result<BogusControlFlowMode, String> {
    let s = s.to_lowercase();
    match s.as_str() {
        "basic" | "v1" => Ok(BogusControlFlowMode::Basic),
        "polaris-primes" | "primes" | "v2" => Ok(BogusControlFlowMode::PolarisPrimes),
        _ => Err(format!("unknown alias access mode: {}", s)),
    }
}

impl Default for BogusControlFlowConfig {
    fn default() -> Self {
        Self {
            enable: false,
            mode: BogusControlFlowMode::Basic,
            probability: 80,
            loop_count: 1,
        }
    }
}

impl EnvOverlay for BogusControlFlowConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_BOGUS_CONTROL_FLOW").is_ok() {
            self.enable = bool_var("AMICE_BOGUS_CONTROL_FLOW", self.enable);
        }
        if let Ok(prob) = std::env::var("AMICE_BOGUS_CONTROL_FLOW_PROB") {
            if let Ok(val) = prob.parse::<u32>() {
                if val <= 100 {
                    self.probability = val;
                } else {
                    warn!("AMICE_BOGUS_CONTROL_FLOW_PROB must be <= 100, got {}, using 100%", val);
                    self.probability = min(100, max(0, val));
                }
            } else {
                warn!("Invalid AMICE_BOGUS_CONTROL_FLOW_PROB value: {}, using default", prob);
            }
        }
        if let Ok(loops) = std::env::var("AMICE_BOGUS_CONTROL_FLOW_LOOPS") {
            if let Ok(val) = loops.parse::<u32>() {
                if val >= 1 {
                    self.loop_count = val;
                } else {
                    warn!(
                        "AMICE_BOGUS_CONTROL_FLOW_LOOPS must be >= 1, got {}, using default",
                        val
                    );
                }
            } else {
                warn!("Invalid AMICE_BOGUS_CONTROL_FLOW_LOOPS value: {}, using default", loops);
            }
        }

        if let Ok(mode) = std::env::var("AMICE_BOGUS_CONTROL_FLOW_MODE") {
            if let Ok(val) = parse_alias_access_mode(&mode) {
                self.mode = val;
            } else {
                warn!("Invalid AMICE_BOGUS_CONTROL_FLOW_MODE value: {}, using default", mode);
            }
        }
    }
}

impl FunctionAnnotationsOverlay for BogusControlFlowConfig {
    type Config = BogusControlFlowConfig;

    fn overlay_annotations<'a>(&self, module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<Self::Config> {
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
            .get_bool("bogus_control_flow")
            .or_else(|| parser.get_bool("boguscfg"))
            .or_else(|| parser.get_bool("bcf")) // 兼容Polaris-Obfuscator
            .map(|v| cfg.enable = v);

        parser
            .get_string("bogus_control_flow_mode")
            .or_else(|| parser.get_string("bcf_mode"))
            .map(|v| cfg.mode = parse_alias_access_mode(&v).unwrap_or_else(|e| {
                warn!("parse bogus control flow mode failed: {}", e);
                BogusControlFlowMode::default()
            }));

        parser
            .get_number("bogus_control_flow_prob")
            .or_else(|| parser.get_number("bcf_prob"))
            .map(|v| cfg.probability = v);

        parser.get_number("bogus_control_flow_loops")
            .or_else(|| parser.get_number("bcf_loops"))
            .map(|v| cfg.loop_count = v);


        Ok(cfg)
    }
}
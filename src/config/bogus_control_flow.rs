use super::bool_var;
use crate::pass_registry::EnvOverlay;
use log::warn;
use serde::{Deserialize, Serialize};
use std::cmp::{max, min};

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

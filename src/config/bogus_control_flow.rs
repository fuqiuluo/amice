use super::bool_var;
use crate::pass_registry::EnvOverlay;
use log::warn;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BogusControlFlowConfig {
    pub enable: bool,
    /// Probability (0-100) that a basic block will be obfuscated
    pub probability: u32,
    /// Number of times to run the obfuscation pass on the function
    pub loop_count: u32,
}

impl Default for BogusControlFlowConfig {
    fn default() -> Self {
        Self {
            enable: false,
            probability: 30,
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
                    warn!(
                        "AMICE_BOGUS_CONTROL_FLOW_PROB must be 0-100, got {}, using default",
                        val
                    );
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
    }
}

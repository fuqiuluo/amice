use super::{EnvOverlay, bool_var};
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
            enable: true,
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

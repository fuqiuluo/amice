use super::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use amice_plugin::inkwell::{module::Module, values::FunctionValue};
use log::warn;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BogusControlFlowConfig {
    pub enable: bool,
    /// Candidate selection probability, clamped to 100.
    pub probability: u32,
    /// Regions per function; hard cap 16. Zero disables rewriting.
    pub max_regions: u32,
    /// Same-width original instructions per region, 2..16.
    pub max_region_instructions: u32,
    /// Add one guarded, forcibly inlined snapshot of the original function.
    pub clone: bool,
    /// Random by default; explicit values provide reproducible diversification.
    pub seed: u64,
}

impl Default for BogusControlFlowConfig {
    fn default() -> Self {
        Self {
            enable: false,
            probability: 80,
            max_regions: 2,
            max_region_instructions: 8,
            clone: true,
            seed: rand::random(),
        }
    }
}

impl EnvOverlay for BogusControlFlowConfig {
    fn overlay_env(&mut self) {
        self.enable = bool_var("AMICE_BOGUS_CONTROL_FLOW", self.enable);
        self.clone = bool_var("AMICE_BOGUS_CONTROL_FLOW_CLONE", self.clone);
        for (name, target) in [
            ("AMICE_BOGUS_CONTROL_FLOW_PROB", &mut self.probability),
            ("AMICE_BOGUS_CONTROL_FLOW_MAX_REGIONS", &mut self.max_regions),
            (
                "AMICE_BOGUS_CONTROL_FLOW_MAX_REGION_INSTRUCTIONS",
                &mut self.max_region_instructions,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                if let Ok(value) = value.parse() {
                    *target = value;
                } else {
                    warn!("Invalid {name}: {value}");
                }
            }
        }
        if let Ok(value) = std::env::var("AMICE_BOGUS_CONTROL_FLOW_SEED") {
            if let Ok(value) = value.parse() {
                self.seed = value;
            } else {
                warn!("Invalid AMICE_BOGUS_CONTROL_FLOW_SEED: {value}");
            }
        }
        for name in ["AMICE_BOGUS_CONTROL_FLOW_MODE", "AMICE_BOGUS_CONTROL_FLOW_LOOPS"] {
            if std::env::var_os(name).is_some() {
                warn!("{name} was removed; BCF now uses bounded digit-state regions");
            }
        }
    }
}

impl FunctionAnnotationsOverlay for BogusControlFlowConfig {
    type Config = Self;

    fn overlay_annotations<'a>(&self, module: &mut Module<'a>, function: FunctionValue<'a>) -> anyhow::Result<Self> {
        let mut cfg = self.clone();
        let annotations = module
            .read_function_annotate(function)
            .map_err(anyhow::Error::msg)?
            .join(" ");
        let mut parser = EloquentConfigParser::new();
        parser.parse(&annotations).map_err(anyhow::Error::msg)?;
        if parser
            .get_string("bogus_control_flow_mode")
            .or_else(|| parser.get_string("bcf_mode"))
            .is_some()
            || parser
                .get_number::<u32>("bogus_control_flow_loops")
                .or_else(|| parser.get_number::<u32>("bcf_loops"))
                .is_some()
        {
            warn!("BCF mode/loops annotations were removed; use bcf_max_regions and bcf_max_region_instructions");
        }
        if let Some(v) = parser
            .get_bool("bogus_control_flow")
            .or_else(|| parser.get_bool("boguscfg"))
            .or_else(|| parser.get_bool("bcf"))
        {
            cfg.enable = v;
        }
        if let Some(v) = parser
            .get_bool("bogus_control_flow_clone")
            .or_else(|| parser.get_bool("bcf_clone"))
        {
            cfg.clone = v;
        }
        for (long, short, target) in [
            ("bogus_control_flow_prob", "bcf_prob", &mut cfg.probability),
            (
                "bogus_control_flow_max_regions",
                "bcf_max_regions",
                &mut cfg.max_regions,
            ),
            (
                "bogus_control_flow_max_region_instructions",
                "bcf_max_region_instructions",
                &mut cfg.max_region_instructions,
            ),
        ] {
            if let Some(v) = parser.get_number(long).or_else(|| parser.get_number(short)) {
                *target = v;
            }
        }
        if let Some(v) = parser
            .get_number::<u64>("bogus_control_flow_seed")
            .or_else(|| parser.get_number::<u64>("bcf_seed"))
        {
            cfg.seed = v;
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bcf_seed_defaults_are_random_and_explicit_values_are_preserved() {
        let defaults = (0..8)
            .map(|_| BogusControlFlowConfig::default().seed)
            .collect::<Vec<_>>();
        assert!(defaults.windows(2).any(|pair| pair[0] != pair[1]));
        let omitted = (0..8)
            .map(|_| serde_json::from_str::<BogusControlFlowConfig>("{}").unwrap().seed)
            .collect::<Vec<_>>();
        assert!(omitted.windows(2).any(|pair| pair[0] != pair[1]));
        for seed in [0, 42, u64::MAX] {
            let cfg: BogusControlFlowConfig = serde_json::from_str(&format!(r#"{{"seed":{seed}}}"#)).unwrap();
            assert_eq!(cfg.seed, seed);
            assert_eq!(cfg.clone().seed, seed);
        }
    }

    #[test]
    fn bcf_region_config_rejects_removed_algorithms() {
        assert!(serde_json::from_str::<BogusControlFlowConfig>(r#"{"mode":"basic"}"#).is_err());
        assert!(serde_json::from_str::<BogusControlFlowConfig>(r#"{"loop_count":2}"#).is_err());
        let cfg: BogusControlFlowConfig = serde_json::from_str(r#"{"enable":true,"max_regions":0,"seed":42}"#).unwrap();
        assert!(cfg.enable);
        assert_eq!(cfg.max_regions, 0);
        assert_eq!(cfg.max_region_instructions, 8);
        assert_eq!(cfg.seed, 42);
    }
}

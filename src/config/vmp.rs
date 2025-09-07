use crate::config::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use bitflags::bitflags;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use log::warn;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VMPConfig {
    pub enable: bool,
    pub flags: VMPFlag,
}

bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct VMPFlag: u32 {
        /// Instruction polymorphism
        const PolyInstruction =             0b00000001;
        /// Automatically clean up useless registers
        const AutoCleanupRegister =         0b00000010;
        /// Type checking (disabled for now)
        const TypeCheck =                   0b00000100;
    }
}

fn parse_vmp_flags(value: &str) -> VMPFlag {
    let mut flags = VMPFlag::empty();
    for x in value.split(',') {
        let x = x.trim().to_lowercase();
        if x.is_empty() {
            continue;
        }
        match x.as_str() {
            "poly_inst" => flags |= VMPFlag::PolyInstruction,
            "auto_cleanup_reg" => flags |= VMPFlag::AutoCleanupRegister,
            "type_check" => flags |= VMPFlag::TypeCheck,
            _ => warn!("Unknown AMICE_VMP_FLAGS: \"{x}\" , ignoring"),
        }
    }
    flags
}

impl EnvOverlay for VMPConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_VMP").is_ok() {
            self.enable = bool_var("AMICE_VMP", self.enable);
        }

        if let Ok(env) = std::env::var("AMICE_VMP_FLAGS") {
            self.flags = parse_vmp_flags(&env);
        }
    }
}

impl FunctionAnnotationsOverlay for VMPConfig {
    type Config = VMPConfig;

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
            .get_bool("vmp")
            .or_else(|| parser.get_bool("xVMP")) // respect!
            .map(|v| cfg.enable = v);

        parser
            .get_bool("vmp_polymorphic")
            .or_else(|| parser.get_bool("vmp_poly"))
            .or_else(|| parser.get_bool("vmp_poly_inst"))
            .map(|v| cfg.flags |= VMPFlag::PolyInstruction);

        parser
            .get_bool("vmp_auto_cleanup_reg")
            .or_else(|| parser.get_bool("vmp_auto_cleanup_register"))
            .map(|v| cfg.flags |= VMPFlag::AutoCleanupRegister);

        parser
            .get_bool("vmp_type_check")
            .map(|v| cfg.flags |= VMPFlag::TypeCheck);

        Ok(cfg)
    }
}

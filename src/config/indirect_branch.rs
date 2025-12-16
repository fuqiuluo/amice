use super::bool_var;
use crate::config::eloquent_config::EloquentConfigParser;
use crate::pass_registry::{EnvOverlay, FunctionAnnotationsOverlay};
use amice_llvm::inkwell2::ModuleExt;
use bitflags::bitflags;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use log::warn;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IndirectBranchFlags: u32 {
        /// Enable basic indirect branch obfuscation
        const Basic =             0b00000001;
        /// Insert dummy blocks to complicate control flow analysis
        const DummyBlock =        0b00000010;
        /// Chain multiple dummy blocks together (includes DummyBlock)
        const ChainedDummyBlock = 0b00000110; // note: includes DummyBlock
        /// Encrypt the block index to obscure the jump target
        const EncryptBlockIndex = 0b00001000;
        /// Insert junk instructions in dummy blocks
        const DummyJunk =         0b00010000;
        /// Shuffle the jump table to randomize basic block order
        const ShuffleTable =      0b00100000;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectBranchConfig {
    /// Whether to enable indirect branch obfuscation
    pub enable: bool,
    /// Configuration flags for different indirect branch techniques
    #[serde(deserialize_with = "deserialize_indirect_branch_flags")]
    pub flags: IndirectBranchFlags,
}

impl Default for IndirectBranchConfig {
    fn default() -> Self {
        Self {
            enable: false,
            flags: IndirectBranchFlags::empty(),
        }
    }
}

fn parse_indirect_branch_flags(value: &str) -> IndirectBranchFlags {
    let mut flags = IndirectBranchFlags::empty();
    for x in value.split(',') {
        let x = x.trim().to_lowercase();
        if x.is_empty() {
            continue;
        }
        match x.as_str() {
            "dummy_block" => flags |= IndirectBranchFlags::DummyBlock,
            "chained_dummy_blocks" => flags |= IndirectBranchFlags::ChainedDummyBlock,
            "encrypt_block_index" => flags |= IndirectBranchFlags::EncryptBlockIndex,
            "dummy_junk" => flags |= IndirectBranchFlags::DummyJunk,
            "shuffle_table" => flags |= IndirectBranchFlags::ShuffleTable,
            _ => warn!("Unknown AMICE_INDIRECT_BRANCH_FLAGS: \"{x}\" , ignoring"),
        }
    }
    flags
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IndirectBranchFlagsRepr {
    Bits(u32),
    One(String),
    Many(Vec<String>),
}

fn deserialize_indirect_branch_flags<'de, D>(deserializer: D) -> Result<IndirectBranchFlags, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let repr = IndirectBranchFlagsRepr::deserialize(deserializer)?;
    let flags = match repr {
        IndirectBranchFlagsRepr::Bits(bits) => IndirectBranchFlags::from_bits_truncate(bits),
        IndirectBranchFlagsRepr::One(s) => parse_indirect_branch_flags(&s),
        IndirectBranchFlagsRepr::Many(arr) => {
            let mut all = IndirectBranchFlags::empty();
            for s in arr {
                all |= parse_indirect_branch_flags(&s);
            }
            all
        },
    };
    Ok(flags)
}

impl EnvOverlay for IndirectBranchConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_INDIRECT_BRANCH").is_ok() {
            self.enable = bool_var("AMICE_INDIRECT_BRANCH", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_INDIRECT_BRANCH_FLAGS") {
            self.flags |= parse_indirect_branch_flags(&v);
        }
    }
}

impl FunctionAnnotationsOverlay for IndirectBranchConfig {
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
            .get_bool("indirect_branch")
            .or_else(|| parser.get_bool("ib"))
            .or_else(|| parser.get_bool("indirectbr")) // 兼容 Polaris-Obfuscator
            .or_else(|| parser.get_bool("indbr")) // 兼容 Arkari
            .map(|v| cfg.enable = v);

        Ok(cfg)
    }
}

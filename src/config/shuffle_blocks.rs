use crate::config::{EnvOverlay, bool_var};
use crate::pass_registry::FunctionAnnotationsOverlay;
use amice_llvm::inkwell2::ModuleExt;
use bitflags::bitflags;
use llvm_plugin::inkwell::module::Module;
use llvm_plugin::inkwell::values::FunctionValue;
use log::warn;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ShuffleBlocksFlags: u32 {
        /// Reverse the order of basic blocks
        const Reverse          = 0b0000_0001;
        /// Randomly shuffle the order of basic blocks
        const Random           = 0b0000_0010;
        /// Rotate basic blocks (left rotate by 1)
        const Rotate           = 0b0000_0100;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShuffleBlocksConfig {
    /// Whether to enable basic block shuffling obfuscation
    pub enable: bool,
    /// Configuration flags for different shuffling techniques
    #[serde(deserialize_with = "deserialize_shuffle_blocks_flags")]
    pub flags: ShuffleBlocksFlags,
}

impl Default for ShuffleBlocksConfig {
    fn default() -> Self {
        Self {
            enable: false,
            flags: ShuffleBlocksFlags::empty(),
        }
    }
}

impl EnvOverlay for ShuffleBlocksConfig {
    fn overlay_env(&mut self) {
        if std::env::var("AMICE_SHUFFLE_BLOCKS").is_ok() {
            self.enable = bool_var("AMICE_SHUFFLE_BLOCKS", self.enable);
        }
        if let Ok(v) = std::env::var("AMICE_SHUFFLE_BLOCKS_FLAGS") {
            self.flags |= parse_shuffle_blocks_flags(&v);
        }
    }
}

fn parse_shuffle_blocks_flags(value: &str) -> ShuffleBlocksFlags {
    let mut flags = ShuffleBlocksFlags::empty();
    for x in value.split(',') {
        let x = x.trim().to_lowercase();
        if x.is_empty() {
            continue;
        }
        match x.as_str() {
            "reverse" | "flip" => flags |= ShuffleBlocksFlags::Reverse,
            "random" | "shuffle" => flags |= ShuffleBlocksFlags::Random,
            "rotate" | "rotate_left" => flags |= ShuffleBlocksFlags::Rotate,
            _ => warn!("Unknown AMICE_SHUFFLE_BLOCKS_FLAGS: \"{x}\" , ignoring"),
        }
    }
    flags
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ShuffleBlocksFlagsRepr {
    Bits(u32),
    One(String),
    Many(Vec<String>),
}

pub(crate) fn deserialize_shuffle_blocks_flags<'de, D>(deserializer: D) -> Result<ShuffleBlocksFlags, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let repr = ShuffleBlocksFlagsRepr::deserialize(deserializer)?;
    let flags = match repr {
        ShuffleBlocksFlagsRepr::Bits(bits) => ShuffleBlocksFlags::from_bits_truncate(bits),
        ShuffleBlocksFlagsRepr::One(s) => parse_shuffle_blocks_flags(&s),
        ShuffleBlocksFlagsRepr::Many(arr) => {
            let mut all = ShuffleBlocksFlags::empty();
            for s in arr {
                all |= parse_shuffle_blocks_flags(&s);
            }
            all
        },
    };
    Ok(flags)
}

impl FunctionAnnotationsOverlay for ShuffleBlocksConfig {
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

        let mut parser = crate::config::eloquent_config::EloquentConfigParser::new();
        parser
            .parse(&annotations_expr)
            .map_err(|e| anyhow::anyhow!("parse function annotations failed: {}", e))?;

        parser.get_bool("shuffle_blocks").map(|v| cfg.enable = v);
        parser.get_number("shuffle_blocks_flags").map(|v| {
            cfg.flags |= ShuffleBlocksFlags::from_bits_truncate(v);
        });
        parser.get_string("shuffle_blocks_flags").map(|v| {
            cfg.flags |= parse_shuffle_blocks_flags(&v);
        });

        Ok(cfg)
    }
}

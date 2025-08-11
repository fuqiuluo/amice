use crate::config::{EnvOverlay, bool_var};
use bitflags::bitflags;
use log::warn;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ShuffleBlocksFlags: u32 {
        const Reverse          = 0b0000_0001; // 反转顺序
        const Random           = 0b0000_0010; // 随机打乱
        const Rotate           = 0b0000_0100; // 循环位移（左移1）
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShuffleBlocksConfig {
    pub enable: bool,
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

pub(crate) fn parse_shuffle_blocks_flags(value: &str) -> ShuffleBlocksFlags {
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

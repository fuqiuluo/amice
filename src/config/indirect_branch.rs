use bitflags::bitflags;
use log::warn;
use serde::{Deserialize, Serialize};
use super::{EnvOverlay, bool_var};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IndirectBranchFlags: u32 {
        const Basic =             0b00000001;
        const DummyBlock =        0b00000010;
        const ChainedDummyBlock = 0b00000110; // note: includes DummyBlock
        const EncryptBlockIndex = 0b00001000;
        const DummyJunk =         0b00010000; // 在 dummy block 中插入干扰性指令
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndirectBranchConfig {
    pub enable: bool,
    #[serde(deserialize_with = "deserialize_indirect_branch_flags")]
    pub flags: IndirectBranchFlags,
}

impl Default for IndirectBranchConfig {
    fn default() -> Self {
        Self {
            enable: true,
            flags: IndirectBranchFlags::empty(),
        }
    }
}

pub(crate) fn parse_indirect_branch_flags(value: &str) -> IndirectBranchFlags {
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

pub(crate) fn deserialize_indirect_branch_flags<'de, D>(deserializer: D) -> Result<IndirectBranchFlags, D::Error>
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
            self.flags |= super::indirect_branch::parse_indirect_branch_flags(&v);
        }
    }
}


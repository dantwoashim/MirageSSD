use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::pattern::PatternKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorpusProfile {
    LargeContainer,
    ManyFiles,
    DedupeVersions,
    RandomRead,
    Mmap,
}

impl FromStr for CorpusProfile {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "large-container" => Ok(Self::LargeContainer),
            "many-files" => Ok(Self::ManyFiles),
            "dedupe-versions" => Ok(Self::DedupeVersions),
            "random-read" => Ok(Self::RandomRead),
            "mmap" => Ok(Self::Mmap),
            _ => Err("unknown corpus profile".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusFile {
    pub path: String,
    pub logical_length: u64,
    pub seed: u64,
    pub pattern: PatternKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusPlan {
    pub schema_version: u32,
    pub profile: CorpusProfile,
    pub seed: u64,
    pub total_logical_bytes: u64,
    pub files: Vec<CorpusFile>,
}

#[must_use]
pub fn plan(profile: CorpusProfile, seed: u64) -> CorpusPlan {
    let files = match profile {
        CorpusProfile::LargeContainer => vec![file(
            "large/container.pak",
            100 * 1024_u64.pow(3),
            seed,
            PatternKind::Pseudorandom,
        )],
        CorpusProfile::ManyFiles => (0_u64..100_000)
            .map(|index| {
                let length = (16 * 1024 * 1024 / (index + 1)).max(64);
                file(
                    &format!("many/{:03}/{index:06}.bin", index % 256),
                    length,
                    seed ^ index,
                    PatternKind::Pseudorandom,
                )
            })
            .collect(),
        CorpusProfile::DedupeVersions => vec![
            file(
                "generation-0/assets.pak",
                8 * 1024_u64.pow(3),
                seed,
                PatternKind::Pseudorandom,
            ),
            file(
                "generation-1/assets.pak",
                8 * 1024_u64.pow(3),
                seed,
                PatternKind::ChangedQuarter,
            ),
        ],
        CorpusProfile::RandomRead => vec![file(
            "oracle/random.bin",
            16 * 1024_u64.pow(3),
            seed,
            PatternKind::Pseudorandom,
        )],
        CorpusProfile::Mmap => vec![file(
            "mmap/aligned.bin",
            4 * 1024_u64.pow(3),
            seed,
            PatternKind::PageRecognizable,
        )],
    };
    let total_logical_bytes = files.iter().fold(0_u64, |total, file| {
        total.saturating_add(file.logical_length)
    });
    CorpusPlan {
        schema_version: 1,
        profile,
        seed,
        total_logical_bytes,
        files,
    }
}

fn file(path: &str, logical_length: u64, seed: u64, pattern: PatternKind) -> CorpusFile {
    CorpusFile {
        path: path.to_string(),
        logical_length,
        seed,
        pattern,
    }
}

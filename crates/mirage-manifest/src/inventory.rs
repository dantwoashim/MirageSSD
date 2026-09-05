use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use mirage_types::{MirageError, MirageErrorKind};
use same_file::Handle;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::path::{validate_relative_inventory_path, windows_case_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparsePolicy {
    RecordAndDoNotFollow,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryEntryKind {
    Directory,
    File,
    ReparsePoint,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    pub relative_path: String,
    pub kind: InventoryEntryKind,
    pub size: u64,
    pub attributes: u32,
    pub read_only: bool,
    pub created_utc_ns: Option<i128>,
    pub modified_utc_ns: Option<i128>,
    pub extension: Option<String>,
    pub reparse_tag: Option<u32>,
    pub hard_link_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub format_version: u32,
    pub entries: Vec<InventoryEntry>,
    pub total_regular_file_bytes: u64,
}

impl Inventory {
    pub fn validate(&self) -> Result<(), MirageError> {
        if self.format_version != 1 {
            return Err(MirageError::unsupported_layout(
                "inventory format version is unsupported",
            ));
        }
        let mut identities = HashSet::new();
        let mut recomputed_total = 0_u64;
        for entry in &self.entries {
            if !entry.relative_path.is_empty() {
                let path = Path::new(&entry.relative_path);
                let canonical = validate_relative_inventory_path(path)?;
                if canonical != entry.relative_path.replace('\\', "/") {
                    return Err(MirageError::manifest_invalid(
                        "inventory path is not in canonical slash form",
                    ));
                }
                let (parent, name) = entry
                    .relative_path
                    .rsplit_once('/')
                    .map_or(("", entry.relative_path.as_str()), |(parent, name)| {
                        (parent, name)
                    });
                if !identities.insert((parent.to_string(), windows_case_key(name))) {
                    return Err(MirageError::manifest_invalid(
                        "inventory contains a Windows case collision",
                    ));
                }
            }
            if entry.kind == InventoryEntryKind::File {
                recomputed_total = recomputed_total.checked_add(entry.size).ok_or_else(|| {
                    MirageError::manifest_invalid("inventory byte total overflows")
                })?;
            }
        }
        if recomputed_total != self.total_regular_file_bytes {
            return Err(MirageError::manifest_invalid(
                "inventory byte total does not match entries",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InventoryScanner {
    reparse_policy: ReparsePolicy,
}

impl Default for InventoryScanner {
    fn default() -> Self {
        Self {
            reparse_policy: ReparsePolicy::RecordAndDoNotFollow,
        }
    }
}

impl InventoryScanner {
    #[must_use]
    pub const fn new(reparse_policy: ReparsePolicy) -> Self {
        Self { reparse_policy }
    }

    pub fn scan(&self, root: &Path) -> Result<Inventory, MirageError> {
        self.scan_with_preflight(root, |_| Ok(()))
    }

    /// Fault-injection hook runs before metadata access and is useful for permission-denial tests.
    pub fn scan_with_preflight(
        &self,
        root: &Path,
        mut preflight: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<Inventory, MirageError> {
        let root_metadata = std::fs::symlink_metadata(root).map_err(safe_io)?;
        if !root_metadata.is_dir()
            || root_metadata.file_type().is_symlink()
            || is_reparse(&root_metadata)
        {
            return Err(MirageError::invalid_argument(
                "inventory root must be a native directory, not a reparse point",
            ));
        }
        let mut entries = Vec::new();
        let mut hard_link_groups: HashMap<Handle, Vec<usize>> = HashMap::new();
        for walked in WalkDir::new(root).follow_links(false).sort_by_file_name() {
            let walked = walked.map_err(|error| {
                MirageError::new(
                    MirageErrorKind::Io,
                    MirageErrorKind::Io.default_code(),
                    "inventory traversal failed",
                )
                .with_source(error)
            })?;
            preflight(walked.path()).map_err(safe_io)?;
            let metadata = std::fs::symlink_metadata(walked.path()).map_err(safe_io)?;
            let relative = walked
                .path()
                .strip_prefix(root)
                .map_err(|_| MirageError::internal_invariant("inventory path escaped its root"))?;
            let relative_path = validate_relative_inventory_path(relative)?;
            let reparse = walked.file_type().is_symlink() || is_reparse(&metadata);
            if reparse && self.reparse_policy == ReparsePolicy::Reject {
                return Err(MirageError::manifest_invalid(
                    "source tree contains a reparse point forbidden by policy",
                ));
            }
            let kind = if reparse {
                InventoryEntryKind::ReparsePoint
            } else if metadata.is_dir() {
                InventoryEntryKind::Directory
            } else if metadata.is_file() {
                InventoryEntryKind::File
            } else {
                InventoryEntryKind::Other
            };
            let entry_index = entries.len();
            entries.push(InventoryEntry {
                extension: walked
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase),
                relative_path,
                kind,
                size: if metadata.is_file() {
                    metadata.len()
                } else {
                    0
                },
                attributes: platform_attributes(&metadata),
                read_only: metadata.permissions().readonly(),
                created_utc_ns: metadata.created().ok().and_then(system_time_ns),
                modified_utc_ns: metadata.modified().ok().and_then(system_time_ns),
                reparse_tag: platform_reparse_tag(&metadata),
                hard_link_count: None,
            });
            if kind == InventoryEntryKind::File {
                hard_link_groups
                    .entry(Handle::from_path(walked.path()).map_err(safe_io)?)
                    .or_default()
                    .push(entry_index);
            }
        }
        for indices in hard_link_groups.values() {
            let observed_count = u64::try_from(indices.len()).map_err(|_| {
                MirageError::manifest_invalid("hard-link count does not fit in u64")
            })?;
            for &index in indices {
                entries[index].hard_link_count = Some(observed_count);
            }
        }
        entries.sort_by(|left, right| {
            windows_case_key(&left.relative_path)
                .cmp(&windows_case_key(&right.relative_path))
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
        let total_regular_file_bytes = entries.iter().try_fold(0_u64, |total, entry| {
            if entry.kind == InventoryEntryKind::File {
                total.checked_add(entry.size)
            } else {
                Some(total)
            }
        });
        let inventory = Inventory {
            format_version: 1,
            entries,
            total_regular_file_bytes: total_regular_file_bytes
                .ok_or_else(|| MirageError::manifest_invalid("inventory byte total overflows"))?,
        };
        inventory.validate()?;
        Ok(inventory)
    }
}

fn safe_io(error: io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "inventory metadata access failed",
    )
    .with_source(error)
}

fn system_time_ns(time: SystemTime) -> Option<i128> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Some(
            i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos()),
        ),
        Err(error) => {
            let duration = error.duration();
            Some(
                -(i128::from(duration.as_secs()) * 1_000_000_000
                    + i128::from(duration.subsec_nanos())),
            )
        }
    }
}

#[cfg(windows)]
fn platform_attributes(metadata: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
}

#[cfg(unix)]
fn platform_attributes(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(any(windows, unix)))]
fn platform_attributes(_: &Metadata) -> u32 {
    0
}

#[cfg(windows)]
fn is_reparse(metadata: &Metadata) -> bool {
    platform_attributes(metadata) & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(_: &Metadata) -> bool {
    false
}

#[cfg(windows)]
fn platform_reparse_tag(metadata: &Metadata) -> Option<u32> {
    is_reparse(metadata).then_some(0)
}

#[cfg(not(windows))]
fn platform_reparse_tag(_: &Metadata) -> Option<u32> {
    None
}

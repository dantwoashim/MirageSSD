use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::inventory::{InventoryEntry, InventoryEntryKind};
use crate::model::FileClass;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationVerdict {
    pub class: FileClass,
    pub eligible_for_virtualization: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationRuleSet {
    mandatory_native_extensions: BTreeSet<String>,
    native_configuration_extensions: BTreeSet<String>,
    virtual_asset_extensions: BTreeSet<String>,
    minimum_virtual_asset_bytes: u64,
}

impl Default for ClassificationRuleSet {
    fn default() -> Self {
        Self {
            mandatory_native_extensions: strings(&[
                "bat", "cmd", "com", "cpl", "dll", "exe", "msi", "ps1", "scr", "sys",
            ]),
            native_configuration_extensions: strings(&[
                "cfg", "ini", "json", "toml", "xml", "yaml", "yml",
            ]),
            virtual_asset_extensions: strings(&[
                "archive", "assets", "ba2", "bsa", "bundle", "cas", "forge", "pak", "ucas", "utoc",
                "vpk", "wad",
            ]),
            minimum_virtual_asset_bytes: 1024 * 1024,
        }
    }
}

impl ClassificationRuleSet {
    #[must_use]
    pub fn with_virtual_asset_extension(mut self, extension: impl Into<String>) -> Self {
        self.virtual_asset_extensions
            .insert(extension.into().to_ascii_lowercase());
        self
    }

    #[must_use]
    pub const fn with_minimum_virtual_asset_bytes(mut self, bytes: u64) -> Self {
        self.minimum_virtual_asset_bytes = bytes;
        self
    }

    #[must_use]
    pub fn classify(&self, entry: &InventoryEntry) -> ClassificationVerdict {
        if entry.kind != InventoryEntryKind::File {
            return verdict(
                FileClass::NativeMutable,
                false,
                "non-regular entries remain native and are never followed",
            );
        }
        let extension = entry.extension.as_deref().unwrap_or_default();
        if self.mandatory_native_extensions.contains(extension) {
            let class = if extension == "dll" {
                FileClass::NativeLibrary
            } else {
                FileClass::NativeExecutable
            };
            return verdict(
                class,
                false,
                "executable and security-sensitive extensions are native",
            );
        }
        if self.native_configuration_extensions.contains(extension) {
            return verdict(
                FileClass::NativeConfiguration,
                false,
                "configuration stays on native NTFS",
            );
        }
        if self.virtual_asset_extensions.contains(extension)
            && entry.size >= self.minimum_virtual_asset_bytes
        {
            return verdict(
                FileClass::VirtualContainer,
                true,
                "configured large immutable asset candidate",
            );
        }
        verdict(
            FileClass::NativeMutable,
            false,
            "unknown or small files default to native storage",
        )
    }
}

fn strings(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn verdict(class: FileClass, eligible: bool, reason: &str) -> ClassificationVerdict {
    ClassificationVerdict {
        class,
        eligible_for_virtualization: eligible,
        reason: reason.to_string(),
    }
}

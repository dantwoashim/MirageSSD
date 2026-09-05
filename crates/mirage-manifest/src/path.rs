use std::path::Path;

use mirage_types::MirageError;

pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_LOGICAL_PATH_BYTES: usize = 32_767;

const RESERVED_DOS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn validate_component(component: &str) -> Result<(), MirageError> {
    if component.is_empty() || component.len() > MAX_COMPONENT_BYTES {
        return Err(MirageError::manifest_invalid(
            "path component is empty or exceeds 255 UTF-8 bytes",
        ));
    }
    if component == "." || component == ".." {
        return Err(MirageError::manifest_invalid(
            "relative path components are forbidden",
        ));
    }
    if component.ends_with(' ') || component.ends_with('.') {
        return Err(MirageError::manifest_invalid(
            "path components cannot end with space or period",
        ));
    }
    if component.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\0' | '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            )
    }) {
        return Err(MirageError::manifest_invalid(
            "path component contains a forbidden Windows character",
        ));
    }
    let base = component.split('.').next().unwrap_or(component);
    if RESERVED_DOS_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
    {
        return Err(MirageError::manifest_invalid(
            "reserved DOS device name is forbidden",
        ));
    }
    Ok(())
}

/// Deterministic portable approximation used for import rejection; Week 4 owns native comparison.
#[must_use]
pub fn windows_case_key(component: &str) -> String {
    component.chars().flat_map(char::to_uppercase).collect()
}

pub(crate) fn validate_relative_inventory_path(path: &Path) -> Result<String, MirageError> {
    let mut logical = String::new();
    for component in path.components() {
        let text = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| MirageError::manifest_invalid("inventory path is not valid UTF-8"))?;
        validate_component(text)?;
        if !logical.is_empty() {
            logical.push('/');
        }
        logical.push_str(text);
        if logical.len() > MAX_LOGICAL_PATH_BYTES {
            return Err(MirageError::manifest_invalid(
                "logical path exceeds 32767 UTF-8 bytes",
            ));
        }
    }
    Ok(logical)
}

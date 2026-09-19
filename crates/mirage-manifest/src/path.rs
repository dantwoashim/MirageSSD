use std::path::Path;

use mirage_types::MirageError;

pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_LOGICAL_PATH_BYTES: usize = 32_767;

/// The canonical Windows naming policy lives in
/// [`mirage_types::fold_name`]; this wrapper preserves the manifest error
/// kind for import validation.
pub fn validate_component(component: &str) -> Result<(), MirageError> {
    if component == "." || component == ".." {
        return Err(MirageError::manifest_invalid(
            "relative path components are forbidden",
        ));
    }
    mirage_types::fold_name(component).map(|_| ()).map_err(|_| {
        MirageError::manifest_invalid("path component is not valid under the Windows naming policy")
    })
}

/// Folded lookup key under the v1 naming policy; invalid input yields an
/// empty key so callers must validate with [`validate_component`] first.
#[must_use]
pub fn windows_case_key(component: &str) -> String {
    mirage_types::fold_name(component).unwrap_or_default()
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

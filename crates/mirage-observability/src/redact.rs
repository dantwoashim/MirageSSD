use std::path::{Path, PathBuf};

pub struct Secret<T>(pub T);
impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Default)]
pub struct RegisteredRoots(Vec<PathBuf>);
impl RegisteredRoots {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self(roots.into_iter().collect())
    }
    #[must_use]
    pub fn display(&self, path: &Path) -> String {
        self.0
            .iter()
            .find_map(|root| path.strip_prefix(root).ok())
            .map_or_else(
                || "[OUTSIDE_REGISTERED_ROOT]".to_owned(),
                |relative| relative.display().to_string(),
            )
    }
}

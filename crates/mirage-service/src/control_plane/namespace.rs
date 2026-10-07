//! Namespace path resolution and pin management.

use super::*;

impl ControlPlaneHandler {
    /// Resolves a mount-relative path (`/` or `\` separated) to a namespace
    /// inode; an empty path resolves to the volume root.
    fn resolve_namespace_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<mirage_types::InodeId, MirageError> {
        let components: Vec<String> = path
            .split(['\\', '/'])
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        if components.is_empty() {
            return self.database.namespace_root(repository_id)?.ok_or_else(|| {
                MirageError::invalid_argument("repository has no managed namespace")
            });
        }
        self.database
            .namespace_resolve_components(repository_id, &components)?
            .ok_or_else(|| MirageError::invalid_argument("path not found in the managed namespace"))
    }

    /// Reconstructs the `/`-joined namespace path of an inode by walking
    /// dirents upward; `None` for the root (listed as "/").
    fn namespace_path_of(
        &self,
        repository_id: RepositoryId,
        inode: mirage_types::InodeId,
    ) -> String {
        let mut names = Vec::new();
        let mut current = inode;
        for _ in 0..128 {
            match self
                .database
                .namespace_entry(repository_id, current)
                .ok()
                .flatten()
            {
                Some((parent, name)) => {
                    names.push(name);
                    current = parent;
                }
                None => break,
            }
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    pub(super) fn namespace_pin_set(
        &self,
        repository_id: RepositoryId,
        path: &str,
        pin: bool,
    ) -> Result<ResponseBody, MirageError> {
        let inode = self.resolve_namespace_path(repository_id, path)?;
        if pin {
            self.database
                .namespace_pin(repository_id, inode, now_ns())?;
        } else if !self.database.namespace_unpin(repository_id, inode)? {
            return Err(MirageError::invalid_argument("path is not pinned"));
        }
        // Refresh the live host's pinned set; an unmounted repo picks the
        // rows up at mount time.
        if let Ok(mut mounts) = self.mounts.lock()
            && mounts.is_running(repository_id).unwrap_or(false)
        {
            let _ = mounts.reload_pins(repository_id);
        }
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "path": path,
            "inode": hex_inode(inode),
            "pinned": pin,
        })))
    }

    pub(super) fn namespace_pins_list(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
        let pins = self.database.namespace_pins(repository_id)?;
        let entries: Vec<_> = pins
            .iter()
            .map(|inode| {
                json!({
                    "inode": hex_inode(*inode),
                    "path": self.namespace_path_of(repository_id, *inode),
                })
            })
            .collect();
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "pins": entries,
        })))
    }
}

fn hex_inode(inode: mirage_types::InodeId) -> String {
    let mut out = String::with_capacity(32);
    for byte in inode.as_bytes() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

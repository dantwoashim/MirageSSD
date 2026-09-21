//! Open-handle tracking for Windows filesystem semantics: share-mode checks,
//! delete-pending, and rename-while-open. The table keys on the durable
//! 128-bit inode so identity survives rename; a delete against an open file
//! becomes a tombstone until the last handle closes.

use std::collections::HashMap;
use std::sync::Mutex;

use mirage_types::{InodeId, MirageError};

/// Share permissions an opener grants to subsequent openers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareAccess {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
}

impl ShareAccess {
    pub const ALL: Self = Self {
        read: true,
        write: true,
        delete: true,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        delete: false,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
        delete: false,
    };
}

/// The access a caller requests when opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesiredAccess {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
}

#[derive(Debug, Clone)]
struct OpenRecord {
    opens: u32,
    share_read: u32,
    share_write: u32,
    share_delete: u32,
    desired_read: u32,
    desired_write: u32,
    desired_delete: u32,
    delete_pending: bool,
}

/// Live handle state for one mounted volume.
#[derive(Default)]
pub struct HandleTable {
    inner: Mutex<HashMap<InodeId, OpenRecord>>,
}

/// What a delete request resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteDisposition {
    /// No open handles — the node was removed.
    Removed,
    /// Open handles remain — the name is unlinked but the inode is a
    /// delete-pending tombstone until the last close.
    Pending,
}

impl HandleTable {
    /// Registers an open. Fails with `AccessDenied`-class conflicts when the
    /// requested access or granted share collides with live opens, or when the
    /// node is a delete-pending tombstone.
    pub fn open(
        &self,
        inode: InodeId,
        desired: DesiredAccess,
        share: ShareAccess,
    ) -> Result<(), MirageError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| MirageError::internal_invariant("handle table lock poisoned"))?;
        if let Some(record) = inner.get_mut(&inode) {
            if record.delete_pending {
                return Err(MirageError::repository_conflict("file is delete-pending"));
            }
            if desired.read && record.share_read < record.opens
                || desired.write && record.share_write < record.opens
                || desired.delete && record.share_delete < record.opens
            {
                return Err(MirageError::repository_conflict(
                    "share access conflicts with an open handle",
                ));
            }
            if share.read {
                record.share_read += 1;
            }
            if share.write {
                record.share_write += 1;
            }
            if share.delete {
                record.share_delete += 1;
            }
            if desired.read {
                record.desired_read += 1;
            }
            if desired.write {
                record.desired_write += 1;
            }
            if desired.delete {
                record.desired_delete += 1;
            }
            record.opens += 1;
            return Ok(());
        }
        inner.insert(
            inode,
            OpenRecord {
                opens: 1,
                share_read: u32::from(share.read),
                share_write: u32::from(share.write),
                share_delete: u32::from(share.delete),
                desired_read: u32::from(desired.read),
                desired_write: u32::from(desired.write),
                desired_delete: u32::from(desired.delete),
                delete_pending: false,
            },
        );
        Ok(())
    }

    /// Closes one open. Returns `true` when the inode's delete-pending
    /// tombstone just became final — the caller must drop the namespace row.
    pub fn close(
        &self,
        inode: InodeId,
        desired: DesiredAccess,
        share: ShareAccess,
    ) -> Result<bool, MirageError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| MirageError::internal_invariant("handle table lock poisoned"))?;
        let Some(record) = inner.get_mut(&inode) else {
            return Ok(false);
        };
        record.opens = record.opens.saturating_sub(1);
        record.share_read = record.share_read.saturating_sub(u32::from(share.read));
        record.share_write = record.share_write.saturating_sub(u32::from(share.write));
        record.share_delete = record.share_delete.saturating_sub(u32::from(share.delete));
        record.desired_read = record.desired_read.saturating_sub(u32::from(desired.read));
        record.desired_write = record
            .desired_write
            .saturating_sub(u32::from(desired.write));
        record.desired_delete = record
            .desired_delete
            .saturating_sub(u32::from(desired.delete));
        if record.opens == 0 {
            let pending = record.delete_pending;
            inner.remove(&inode);
            return Ok(pending);
        }
        Ok(false)
    }

    /// A delete request against the inode: `Removed` when unopened (the
    /// caller deletes the row now), `Pending` when handles hold it open —
    /// the tombstone persists until the last close returns `true`.
    pub fn request_delete(&self, inode: InodeId) -> Result<DeleteDisposition, MirageError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| MirageError::internal_invariant("handle table lock poisoned"))?;
        match inner.get_mut(&inode) {
            Some(record) => {
                if record.desired_delete == 0 {
                    return Err(MirageError::repository_conflict(
                        "open handles did not grant delete sharing",
                    ));
                }
                record.delete_pending = true;
                Ok(DeleteDisposition::Pending)
            }
            None => Ok(DeleteDisposition::Removed),
        }
    }

    /// True while the inode has at least one open handle.
    #[must_use]
    pub fn is_open(&self, inode: InodeId) -> bool {
        self.inner
            .lock()
            .ok()
            .is_some_and(|inner| inner.contains_key(&inode))
    }

    /// True while the inode is an open delete-pending tombstone.
    #[must_use]
    pub fn is_delete_pending(&self, inode: InodeId) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.get(&inode).map(|record| record.delete_pending))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inode(byte: u8) -> InodeId {
        InodeId::from_bytes([byte; 16])
    }

    #[test]
    fn delete_with_open_handle_is_pending_until_last_close() {
        let table = HandleTable::default();
        table
            .open(
                inode(1),
                DesiredAccess {
                    read: true,
                    write: false,
                    delete: true,
                },
                ShareAccess::ALL,
            )
            .unwrap();
        assert_eq!(
            table.request_delete(inode(1)).unwrap(),
            DeleteDisposition::Pending
        );
        assert!(table.is_delete_pending(inode(1)));
        // A second open against a tombstone is denied.
        assert!(
            table
                .open(
                    inode(1),
                    DesiredAccess {
                        read: true,
                        write: false,
                        delete: false,
                    },
                    ShareAccess::ALL,
                )
                .is_err()
        );
        assert!(
            table
                .close(
                    inode(1),
                    DesiredAccess {
                        read: true,
                        write: false,
                        delete: true,
                    },
                    ShareAccess::ALL,
                )
                .unwrap()
        );
        assert!(!table.is_delete_pending(inode(1)));
    }

    #[test]
    fn share_conflicts_are_explicit_and_delete_requires_share_delete() {
        let table = HandleTable::default();
        table
            .open(
                inode(2),
                DesiredAccess {
                    read: true,
                    write: true,
                    delete: false,
                },
                ShareAccess::READ, // write is unshared, delete unshared
            )
            .unwrap();
        // Delete is refused: the open handle never granted delete sharing.
        assert!(table.request_delete(inode(2)).is_err());
        // A second opener wanting write collides with the unshared write.
        assert!(
            table
                .open(
                    inode(2),
                    DesiredAccess {
                        read: false,
                        write: true,
                        delete: false,
                    },
                    ShareAccess::READ_WRITE,
                )
                .is_err()
        );
        // Read access is still fine.
        table
            .open(
                inode(2),
                DesiredAccess {
                    read: true,
                    write: false,
                    delete: false,
                },
                ShareAccess::READ_WRITE,
            )
            .unwrap();
    }
}

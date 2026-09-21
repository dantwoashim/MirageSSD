//! Versioned managed-namespace documents: immutable checkpoints and bounded
//! deltas with deterministic encoding, so checkpoint identity is a content
//! hash and a delta is provably derived from its base checkpoint.

use mirage_types::{CommitHash, InodeId, MirageError, RepositoryId};

/// Format version of namespace checkpoint and delta documents. Readers must
/// reject unknown versions rather than guess; a managed volume whose
/// namespace version is newer mounts read-only through the legacy snapshot.
pub const NAMESPACE_FORMAT_VERSION: u32 = 1;
/// A single delta is bounded; the writer checkpoints when the bound is hit.
pub const MAX_DELTA_OPS: usize = 8_192;
/// Upper bound on a decoded namespace document, mirroring the manifest bound.
pub const MAX_NAMESPACE_DOCUMENT_BYTES: usize = 512 * 1024 * 1024;
/// Upper bound on a single delta document.
pub const MAX_DELTA_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

const CHECKPOINT_MAGIC: &[u8; 4] = b"MNSC";
const DELTA_MAGIC: &[u8; 4] = b"MNSD";

/// One node of a namespace checkpoint. Inodes are name-independent;
/// `display_name` preserves the spelling while `folded_name` is the lookup key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceNodeRecord {
    pub inode: InodeId,
    pub parent: Option<InodeId>,
    pub folded_name: String,
    pub display_name: String,
    pub directory: bool,
    pub size: u64,
    pub version_root: Option<[u8; 32]>,
    pub extent_root: Option<[u8; 32]>,
}

/// An immutable namespace snapshot. Nodes are stored sorted by inode so the
/// document is canonical and its hash is stable.
#[derive(Debug, Clone)]
pub struct NamespaceCheckpoint {
    pub volume_id: RepositoryId,
    pub checkpoint_seq: u64,
    /// Commit whose manifest this checkpoint reflects, when published.
    pub parent_commit: Option<CommitHash>,
    pub nodes: Vec<NamespaceNodeRecord>,
}

/// One mutation applied on top of a base checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceOp {
    Create {
        parent: InodeId,
        inode: InodeId,
        display_name: String,
        folded_name: String,
        directory: bool,
        size: u64,
    },
    Rename {
        inode: InodeId,
        from_parent: InodeId,
        from_folded: String,
        to_parent: InodeId,
        display_name: String,
        folded_name: String,
    },
    Delete {
        parent: InodeId,
        folded_name: String,
        inode: InodeId,
    },
    SetRoots {
        inode: InodeId,
        size: u64,
        version_root: Option<[u8; 32]>,
        extent_root: Option<[u8; 32]>,
    },
}

/// A bounded, hash-addressable delta against a base checkpoint. `base_hash`
/// is the BLAKE3 of the base checkpoint document, so a delta presented with
/// a mismatched base is rejected rather than misapplied.
#[derive(Debug, Clone)]
pub struct NamespaceDelta {
    pub volume_id: RepositoryId,
    pub base_document_hash: [u8; 32],
    pub delta_seq: u64,
    pub ops: Vec<NamespaceOp>,
}

struct Sink {
    bytes: Vec<u8>,
}

impl Sink {
    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }
    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
    fn text(&mut self, value: &str) -> Result<(), MirageError> {
        let length = u32::try_from(value.len())
            .map_err(|_| MirageError::manifest_invalid("namespace name exceeds document bound"))?;
        self.u32(length);
        self.raw(value.as_bytes());
        Ok(())
    }
    fn opt16(&mut self, value: &Option<InodeId>) {
        match value {
            Some(inode) => {
                self.byte(1);
                self.raw(inode.as_bytes());
            }
            None => self.byte(0),
        }
    }
    fn opt32(&mut self, value: &Option<[u8; 32]>) {
        match value {
            Some(bytes) => {
                self.byte(1);
                self.raw(bytes);
            }
            None => self.byte(0),
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], MirageError> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| MirageError::manifest_invalid("namespace document is truncated"))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }
    fn byte(&mut self) -> Result<u8, MirageError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, MirageError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn u64(&mut self) -> Result<u64, MirageError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    fn inode(&mut self) -> Result<InodeId, MirageError> {
        Ok(InodeId::from_bytes(
            self.take(16)?.try_into().expect("16 bytes"),
        ))
    }
    fn text(&mut self) -> Result<String, MirageError> {
        let length = self.u32()? as usize;
        if length > 1024 {
            return Err(MirageError::manifest_invalid(
                "namespace name exceeds the component bound",
            ));
        }
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| MirageError::manifest_invalid("namespace name is not valid UTF-8"))
    }
    fn opt16(&mut self) -> Result<Option<InodeId>, MirageError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.inode()?)),
            _ => Err(MirageError::manifest_invalid(
                "namespace optional flag is invalid",
            )),
        }
    }
    fn opt32(&mut self) -> Result<Option<[u8; 32]>, MirageError> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.take(32)?.try_into().expect("32 bytes"))),
            _ => Err(MirageError::manifest_invalid(
                "namespace optional flag is invalid",
            )),
        }
    }
    fn done(&self) -> Result<(), MirageError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(MirageError::manifest_invalid(
                "namespace document has trailing bytes",
            ))
        }
    }
}

fn check_magic(
    cursor: &mut Cursor<'_>,
    magic: &[u8; 4],
    maximum: usize,
) -> Result<(), MirageError> {
    if cursor.bytes.len() > maximum {
        return Err(MirageError::manifest_invalid(
            "namespace document exceeds its decode bound",
        ));
    }
    if cursor.take(4)? != magic {
        return Err(MirageError::manifest_invalid(
            "namespace document magic is unknown",
        ));
    }
    let version = cursor.u32()?;
    if version != NAMESPACE_FORMAT_VERSION {
        return Err(MirageError::new(
            mirage_types::MirageErrorKind::UnsupportedLayout,
            mirage_types::MirageErrorKind::UnsupportedLayout.default_code(),
            "namespace document was written by an incompatible format version",
        ));
    }
    Ok(())
}

/// Encodes a checkpoint canonically: nodes sorted by inode.
pub fn encode_checkpoint(checkpoint: &NamespaceCheckpoint) -> Result<Vec<u8>, MirageError> {
    let mut nodes = checkpoint.nodes.clone();
    nodes.sort_by(|a, b| a.inode.as_bytes().cmp(b.inode.as_bytes()));
    for pair in nodes.windows(2) {
        if pair[0].inode == pair[1].inode {
            return Err(MirageError::manifest_invalid(
                "namespace checkpoint contains a duplicate inode",
            ));
        }
    }
    let mut sink = Sink { bytes: Vec::new() };
    sink.raw(CHECKPOINT_MAGIC);
    sink.u32(NAMESPACE_FORMAT_VERSION);
    sink.raw(checkpoint.volume_id.as_bytes());
    sink.u64(checkpoint.checkpoint_seq);
    match &checkpoint.parent_commit {
        Some(hash) => {
            sink.byte(1);
            sink.raw(hash.as_bytes());
        }
        None => sink.byte(0),
    }
    sink.u32(
        u32::try_from(nodes.len())
            .map_err(|_| MirageError::manifest_invalid("namespace node count overflows"))?,
    );
    for node in &nodes {
        sink.raw(node.inode.as_bytes());
        sink.opt16(&node.parent);
        sink.text(&node.folded_name)?;
        sink.text(&node.display_name)?;
        sink.byte(u8::from(node.directory));
        sink.u64(node.size);
        sink.opt32(&node.version_root);
        sink.opt32(&node.extent_root);
    }
    Ok(sink.bytes)
}

/// Content hash of an encoded checkpoint document.
#[must_use]
pub fn checkpoint_hash(encoded: &[u8]) -> [u8; 32] {
    *blake3::hash(encoded).as_bytes()
}

/// Decodes a checkpoint document with bounds and canonical-order checks.
pub fn decode_checkpoint(encoded: &[u8]) -> Result<NamespaceCheckpoint, MirageError> {
    let mut cursor = Cursor {
        bytes: encoded,
        at: 0,
    };
    check_magic(&mut cursor, CHECKPOINT_MAGIC, MAX_NAMESPACE_DOCUMENT_BYTES)?;
    let volume_id = RepositoryId::from_bytes(cursor.take(16)?.try_into().expect("16 bytes"));
    let checkpoint_seq = cursor.u64()?;
    let parent_commit = match cursor.byte()? {
        0 => None,
        1 => Some(CommitHash::from_bytes(
            cursor.take(32)?.try_into().expect("32 bytes"),
        )),
        _ => {
            return Err(MirageError::manifest_invalid(
                "namespace checkpoint commit flag is invalid",
            ));
        }
    };
    let count = cursor.u32()? as usize;
    // A node costs at least 36 bytes (inode + parent flag + two length
    // fields + kind + size + two optional flags) — a claimed count that the
    // remaining bytes cannot cover is rejected before allocating.
    const MIN_NODE_BYTES: usize = 16 + 1 + 4 + 4 + 1 + 8 + 1 + 1;
    if count
        .checked_mul(MIN_NODE_BYTES)
        .is_none_or(|minimum| minimum > cursor.bytes.len() - cursor.at)
    {
        return Err(MirageError::manifest_invalid(
            "namespace node count exceeds the document bound",
        ));
    }
    let mut nodes = Vec::with_capacity(count.min(1 << 20));
    let mut previous: Option<InodeId> = None;
    for _ in 0..count {
        let inode = cursor.inode()?;
        if previous.is_some_and(|last| inode.as_bytes() <= last.as_bytes()) {
            return Err(MirageError::manifest_invalid(
                "namespace checkpoint inodes are not strictly ordered",
            ));
        }
        previous = Some(inode);
        let parent = cursor.opt16()?;
        let folded_name = cursor.text()?;
        let display_name = cursor.text()?;
        let directory = match cursor.byte()? {
            0 => false,
            1 => true,
            _ => {
                return Err(MirageError::manifest_invalid(
                    "namespace node kind flag is invalid",
                ));
            }
        };
        let size = cursor.u64()?;
        let version_root = cursor.opt32()?;
        let extent_root = cursor.opt32()?;
        nodes.push(NamespaceNodeRecord {
            inode,
            parent,
            folded_name,
            display_name,
            directory,
            size,
            version_root,
            extent_root,
        });
    }
    cursor.done()?;
    // Graph integrity: the checkpoint must describe exactly one rooted tree.
    let known: std::collections::HashMap<[u8; 16], &NamespaceNodeRecord> = nodes
        .iter()
        .map(|node| (*node.inode.as_bytes(), node))
        .collect();
    let mut roots = 0usize;
    for node in &nodes {
        match node.parent {
            None => {
                if !node.directory {
                    return Err(MirageError::manifest_invalid(
                        "namespace checkpoint root is not a directory",
                    ));
                }
                roots += 1;
            }
            Some(parent) => {
                if parent == node.inode {
                    return Err(MirageError::manifest_invalid(
                        "namespace checkpoint node is its own parent",
                    ));
                }
                let parent_node = known.get(parent.as_bytes()).ok_or_else(|| {
                    MirageError::manifest_invalid(
                        "namespace checkpoint references a missing parent",
                    )
                })?;
                if !parent_node.directory {
                    return Err(MirageError::manifest_invalid(
                        "namespace checkpoint parent is not a directory",
                    ));
                }
            }
        }
    }
    if roots != 1 {
        return Err(MirageError::manifest_invalid(
            "namespace checkpoint must contain exactly one root",
        ));
    }
    // Sibling folded names must be unique — a duplicated key makes lookups
    // ambiguous.
    let mut sibling_names: std::collections::HashSet<([u8; 16], &[u8])> =
        std::collections::HashSet::new();
    for node in &nodes {
        let parent_key = node
            .parent
            .map(|parent| *parent.as_bytes())
            .unwrap_or([0; 16]);
        if !sibling_names.insert((parent_key, node.folded_name.as_bytes())) {
            return Err(MirageError::manifest_invalid(
                "namespace checkpoint duplicates a folded sibling name",
            ));
        }
    }
    // Acyclicity: walking each node's ancestor chain must reach the root in
    // fewer steps than the node count; anything longer is a cycle.
    for node in &nodes {
        let mut at = node;
        let mut hops = 0usize;
        while let Some(parent) = at.parent {
            at = known[parent.as_bytes()];
            hops += 1;
            if hops >= nodes.len() {
                return Err(MirageError::manifest_invalid(
                    "namespace checkpoint contains a parent cycle",
                ));
            }
        }
    }
    Ok(NamespaceCheckpoint {
        volume_id,
        checkpoint_seq,
        parent_commit,
        nodes,
    })
}

/// Encodes a bounded delta document.
pub fn encode_delta(delta: &NamespaceDelta) -> Result<Vec<u8>, MirageError> {
    if delta.ops.len() > MAX_DELTA_OPS {
        return Err(MirageError::manifest_invalid(
            "namespace delta exceeds the operation bound",
        ));
    }
    let mut sink = Sink { bytes: Vec::new() };
    sink.raw(DELTA_MAGIC);
    sink.u32(NAMESPACE_FORMAT_VERSION);
    sink.raw(delta.volume_id.as_bytes());
    sink.raw(&delta.base_document_hash);
    sink.u64(delta.delta_seq);
    sink.u32(
        u32::try_from(delta.ops.len())
            .map_err(|_| MirageError::manifest_invalid("namespace delta op count overflows"))?,
    );
    for op in &delta.ops {
        match op {
            NamespaceOp::Create {
                parent,
                inode,
                display_name,
                folded_name,
                directory,
                size,
            } => {
                sink.byte(0);
                sink.raw(parent.as_bytes());
                sink.raw(inode.as_bytes());
                sink.text(display_name)?;
                sink.text(folded_name)?;
                sink.byte(u8::from(*directory));
                sink.u64(*size);
            }
            NamespaceOp::Rename {
                inode,
                from_parent,
                from_folded,
                to_parent,
                display_name,
                folded_name,
            } => {
                sink.byte(1);
                sink.raw(inode.as_bytes());
                sink.raw(from_parent.as_bytes());
                sink.text(from_folded)?;
                sink.raw(to_parent.as_bytes());
                sink.text(display_name)?;
                sink.text(folded_name)?;
            }
            NamespaceOp::Delete {
                parent,
                folded_name,
                inode,
            } => {
                sink.byte(2);
                sink.raw(parent.as_bytes());
                sink.text(folded_name)?;
                sink.raw(inode.as_bytes());
            }
            NamespaceOp::SetRoots {
                inode,
                size,
                version_root,
                extent_root,
            } => {
                sink.byte(3);
                sink.raw(inode.as_bytes());
                sink.u64(*size);
                sink.opt32(version_root);
                sink.opt32(extent_root);
            }
        }
    }
    Ok(sink.bytes)
}

/// Decodes a bounded delta document.
pub fn decode_delta(encoded: &[u8]) -> Result<NamespaceDelta, MirageError> {
    let mut cursor = Cursor {
        bytes: encoded,
        at: 0,
    };
    check_magic(&mut cursor, DELTA_MAGIC, MAX_DELTA_DOCUMENT_BYTES)?;
    let volume_id = RepositoryId::from_bytes(cursor.take(16)?.try_into().expect("16 bytes"));
    let base_document_hash: [u8; 32] = cursor.take(32)?.try_into().expect("32 bytes");
    let delta_seq = cursor.u64()?;
    let count = cursor.u32()? as usize;
    if count > MAX_DELTA_OPS {
        return Err(MirageError::manifest_invalid(
            "namespace delta exceeds the operation bound",
        ));
    }
    let mut ops = Vec::with_capacity(count.min(MAX_DELTA_OPS));
    for _ in 0..count {
        ops.push(match cursor.byte()? {
            0 => NamespaceOp::Create {
                parent: cursor.inode()?,
                inode: cursor.inode()?,
                display_name: cursor.text()?,
                folded_name: cursor.text()?,
                directory: cursor.byte()? != 0,
                size: cursor.u64()?,
            },
            1 => NamespaceOp::Rename {
                inode: cursor.inode()?,
                from_parent: cursor.inode()?,
                from_folded: cursor.text()?,
                to_parent: cursor.inode()?,
                display_name: cursor.text()?,
                folded_name: cursor.text()?,
            },
            2 => NamespaceOp::Delete {
                parent: cursor.inode()?,
                folded_name: cursor.text()?,
                inode: cursor.inode()?,
            },
            3 => NamespaceOp::SetRoots {
                inode: cursor.inode()?,
                size: cursor.u64()?,
                version_root: cursor.opt32()?,
                extent_root: cursor.opt32()?,
            },
            _ => {
                return Err(MirageError::manifest_invalid(
                    "namespace delta operation tag is unknown",
                ));
            }
        });
    }
    cursor.done()?;
    Ok(NamespaceDelta {
        volume_id,
        base_document_hash,
        delta_seq,
        ops,
    })
}

/// Verifies a delta belongs to the checkpoint chain: volume and base hash
/// must match the checkpoint document it claims to extend.
pub fn delta_matches_checkpoint(
    delta: &NamespaceDelta,
    base_document: &[u8],
    volume_id: RepositoryId,
) -> Result<(), MirageError> {
    if delta.volume_id != volume_id || delta.base_document_hash != checkpoint_hash(base_document) {
        return Err(MirageError::integrity_mismatch(
            "namespace delta does not extend the claimed checkpoint",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inode(byte: u8) -> InodeId {
        InodeId::from_bytes([byte; 16])
    }

    #[test]
    fn checkpoint_round_trip_is_canonical() {
        let volume = RepositoryId::from_bytes([9; 16]);
        let checkpoint = NamespaceCheckpoint {
            volume_id: volume,
            checkpoint_seq: 3,
            parent_commit: Some(CommitHash::from_bytes([5; 32])),
            // Intentionally unordered input; the encoder sorts by inode.
            nodes: vec![
                NamespaceNodeRecord {
                    inode: inode(2),
                    parent: Some(inode(1)),
                    folded_name: "B".into(),
                    display_name: "b".into(),
                    directory: false,
                    size: 7,
                    version_root: Some([1; 32]),
                    extent_root: None,
                },
                NamespaceNodeRecord {
                    inode: inode(1),
                    parent: None,
                    folded_name: String::new(),
                    display_name: String::new(),
                    directory: true,
                    size: 0,
                    version_root: None,
                    extent_root: None,
                },
            ],
        };
        let encoded = encode_checkpoint(&checkpoint).unwrap();
        let decoded = decode_checkpoint(&encoded).unwrap();
        assert_eq!(decoded.volume_id, volume);
        assert_eq!(decoded.checkpoint_seq, 3);
        assert_eq!(decoded.nodes[0].inode, inode(1));
        assert_eq!(decoded.nodes[1].inode, inode(2));
        assert_eq!(checkpoint_hash(&encoded), checkpoint_hash(&encoded));
    }

    fn root() -> NamespaceNodeRecord {
        NamespaceNodeRecord {
            inode: inode(0xFF),
            parent: None,
            folded_name: String::new(),
            display_name: String::new(),
            directory: true,
            size: 0,
            version_root: None,
            extent_root: None,
        }
    }

    #[test]
    fn unknown_version_and_trailing_bytes_are_rejected() {
        let volume = RepositoryId::from_bytes([9; 16]);
        let checkpoint = NamespaceCheckpoint {
            volume_id: volume,
            checkpoint_seq: 0,
            parent_commit: None,
            nodes: vec![root()],
        };
        let mut encoded = encode_checkpoint(&checkpoint).unwrap();
        encoded[5] = 99; // bump format_version field
        let error = decode_checkpoint(&encoded).unwrap_err();
        assert_eq!(error.kind, mirage_types::MirageErrorKind::UnsupportedLayout);
        let mut padded = encode_checkpoint(&checkpoint).unwrap();
        padded.push(0);
        assert!(decode_checkpoint(&padded).is_err());
    }

    #[test]
    fn delta_round_trip_and_base_binding() {
        let volume = RepositoryId::from_bytes([4; 16]);
        let base = encode_checkpoint(&NamespaceCheckpoint {
            volume_id: volume,
            checkpoint_seq: 0,
            parent_commit: None,
            nodes: vec![root()],
        })
        .unwrap();
        let delta = NamespaceDelta {
            volume_id: volume,
            base_document_hash: checkpoint_hash(&base),
            delta_seq: 7,
            ops: vec![
                NamespaceOp::Create {
                    parent: inode(1),
                    inode: inode(2),
                    display_name: "x.txt".into(),
                    folded_name: "X.TXT".into(),
                    directory: false,
                    size: 5,
                },
                NamespaceOp::Rename {
                    inode: inode(2),
                    from_parent: inode(1),
                    from_folded: "X.TXT".into(),
                    to_parent: inode(1),
                    display_name: "y.txt".into(),
                    folded_name: "Y.TXT".into(),
                },
                NamespaceOp::Delete {
                    parent: inode(1),
                    folded_name: "Y.TXT".into(),
                    inode: inode(2),
                },
                NamespaceOp::SetRoots {
                    inode: inode(2),
                    size: 9,
                    version_root: Some([3; 32]),
                    extent_root: Some([4; 32]),
                },
            ],
        };
        let encoded = encode_delta(&delta).unwrap();
        let decoded = decode_delta(&encoded).unwrap();
        assert_eq!(decoded.ops, delta.ops);
        delta_matches_checkpoint(&decoded, &base, volume).unwrap();
        // A delta bound to a different checkpoint is fenced off.
        let other = encode_checkpoint(&NamespaceCheckpoint {
            volume_id: volume,
            checkpoint_seq: 1,
            parent_commit: None,
            nodes: vec![],
        })
        .unwrap();
        assert!(delta_matches_checkpoint(&decoded, &other, volume).is_err());
        assert!(
            delta_matches_checkpoint(&decoded, &base, RepositoryId::from_bytes([8; 16])).is_err()
        );
    }
}

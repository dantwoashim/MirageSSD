#![allow(unsafe_code)]

use handles::{DecodedPageCache, Entry, ViolationLog};
pub use handles::{MirageEngineHandle, MirageFileHandle};
pub use status::MirageStatus;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex};
use mirage_index::{FileView, MountIndex, NodeIndex, PageView, ResolvedSpan};
use mirage_pack::{PackReadEncryption, PackReader, PlainPage};
use mirage_scheduler::{FetchPriority, FlightFailure, FlightMap, PageFlight};
use mirage_types::{
    ByteCount, FetchFailureCause, InodeId, MirageError, MirageErrorKind, PageHash, RepositoryId,
};

pub mod engine_create;
pub mod engine_ops;
pub mod metadata;
pub mod mutation;
pub mod read;
#[cfg(test)]
mod tests;
pub mod write;

pub use engine_create::mirage_engine_create_cache;
pub use engine_create::mirage_engine_create_cache_with_origin;
pub use engine_create::mirage_engine_create_empty;
pub use engine_create::mirage_engine_create_index;
pub use engine_create::mirage_engine_create_local;
pub use engine_create::mirage_engine_create_managed;
pub use engine_create::mirage_engine_create_managed_drive;
pub use engine_create::mirage_engine_create_managed_drive_at;
pub use engine_create::mirage_engine_create_managed_local_provider;
pub use engine_ops::MiragePublicationStats;
pub use engine_ops::mirage_engine_abandon_for_tests;
pub use engine_ops::mirage_engine_compact;
pub use engine_ops::mirage_engine_destroy;
pub use engine_ops::mirage_engine_dirty_free;
pub use engine_ops::mirage_engine_dirty_used_for_tests;
pub use engine_ops::mirage_engine_epoch;
pub use engine_ops::mirage_engine_evict_published;
pub use engine_ops::mirage_engine_mark_mounted;
pub use engine_ops::mirage_engine_publication_stats;
pub use engine_ops::mirage_engine_quiesce;
pub use engine_ops::mirage_engine_reload_pins;
pub use engine_ops::mirage_engine_set_disk_floor;
pub use engine_ops::mirage_engine_set_drive_token;
pub use metadata::MirageEnumerateCallback;
pub use metadata::MirageFileInfo;
pub use metadata::mirage_enumerate;
pub use metadata::mirage_file_close;
pub use metadata::mirage_file_stat;
pub use metadata::mirage_set_times;
pub use mutation::mirage_namespace_create;
pub use mutation::mirage_namespace_delete;
pub use mutation::mirage_namespace_rename;
use mutation::now_ns_i64;
pub use read::mirage_lookup;
pub use read::mirage_read;
pub use read::mirage_read_ex;
pub use read::mirage_read_speculative;
use write::build_extent_mutation_commit;
pub use write::mirage_flush;
pub use write::mirage_truncate;
pub use write::mirage_write;

pub mod directory_backend;

pub mod handles;

pub mod namespace;

pub mod publisher;

mod segments;
mod trace;

pub mod status;

fn contained(operation: impl FnOnce() -> MirageStatus) -> MirageStatus {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(MirageStatus::Internal)
}

fn safe_object_id(value: &str) -> bool {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || value.contains([':', '/', '\\'])
        || value.ends_with(['.', ' '])
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return false;
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

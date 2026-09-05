//! Deterministic immutable mount-index compilation and allocation-free table views.

#![deny(unsafe_code)]

pub mod checked_slice;
pub mod compiler;
pub mod enumerate;
pub mod format;
pub mod header;
pub mod lookup;
mod mapped_file;
pub mod name;
pub mod reader;
pub mod record;
pub mod resolve;
pub mod string_table;
pub mod view;

pub use compiler::{compile_to_bytes, compile_to_path};
pub use enumerate::DirectoryEntry;
pub use format::{FORMAT_VERSION, HEADER_SIZE, MAGIC, Section, SectionKind};
pub use header::Header;
pub use lookup::NodeIndex;
pub use reader::MountIndex;
pub use resolve::{ResolvedSpan, resolve_range};
pub use view::{DirectoryView, ExtentView, FileView, PageView, RemoteLocationView};

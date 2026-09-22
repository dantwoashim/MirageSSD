//! Deterministic logical paging and immutable, independently verifiable pack files.

#![forbid(unsafe_code)]

pub mod encode;
pub mod encrypted_frame;
pub mod footer;
pub mod format;
pub mod frame;
pub mod import;
pub mod index;
pub mod page;
pub mod pager;
pub mod range_plan;
pub mod reader;
pub mod writer;

pub use encrypted_frame::{
    EncryptedFrameAad, decode_encrypted_frame, encode_encrypted_frame, encrypted_frame_pack_id,
};
pub use frame::{DecodedFrame, decode_plain_frame, encode_plain_frame};
pub use import::{
    ImportPlan, ImportReport, ImportedRepository, PlannedFile, import_local,
    import_local_allow_empty, import_local_with_cancel,
};
pub use index::PackEntry;
pub use page::PlainPage;
pub use pager::{PageIter, SourcePager, page_file, page_path, page_path_streaming};
pub use range_plan::{PlannedRange, plan_ranges};
pub use reader::{PackReadEncryption, PackReader};
pub use writer::{CompletedPack, PackEncryption, PackWriter, PackWriterOptions};

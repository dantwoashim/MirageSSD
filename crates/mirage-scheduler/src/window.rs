use mirage_backend::RemoteObjectRef;
use mirage_types::{CheckedRange, PageHash};

use crate::FetchPriority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFrame {
    pub page_hash: PageHash,
    pub object: RemoteObjectRef,
    pub range: CheckedRange,
    pub priority: FetchPriority,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameMapping {
    pub page_hash: PageHash,
    pub window_offset: u64,
    pub encoded_length: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchWindow {
    pub object: RemoteObjectRef,
    pub range: CheckedRange,
    pub priority: FetchPriority,
    pub frames: Vec<FrameMapping>,
    pub gap_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    CacheAfterPayloadFlush,
    CacheBeforeMetadataCommit,
    UpdateAfterJournalFlush,
    UpdateAfterCommitUpload,
    MountBeforeActivation,
    BackendAfterRangeHeaders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    ReturnError,
    ShortIo,
    CorruptBuffer,
    DropConnection,
    DiskFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MirageStatus {
    Ok = 0,
    InvalidArgument = 1,
    NotFound = 2,
    AccessDenied = 3,
    WouldBlock = 4,
    Cancelled = 5,
    IntegrityFailure = 6,
    BackendUnavailable = 7,
    IoError = 8,
    Internal = 255,
}

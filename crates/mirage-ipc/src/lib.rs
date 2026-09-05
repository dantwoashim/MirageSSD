//! Versioned bounded local control protocol.
#![forbid(unsafe_code)]
pub mod auth;
pub mod frame;
pub mod rate_limit;
pub mod request;
pub mod response;
pub use auth::{Authorization, Principal, PrincipalRole};
pub use frame::{MAX_FRAME_BYTES, PROTOCOL_VERSION, decode_frame, encode_frame};
pub use rate_limit::TokenBucket;
pub use request::{Command, DriveQuotaSnapshot, Request, SensitiveString};
pub use response::{Response, ResponseBody};

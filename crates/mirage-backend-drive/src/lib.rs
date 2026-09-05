//! Google Drive immutable-object backend.
#![forbid(unsafe_code)]
pub mod backend;
pub mod client;
pub mod discover;
pub mod error;
pub mod http;
pub mod lookup;
pub mod metadata;
pub mod native_http;
pub mod oauth;
pub mod quota;
pub mod read;
pub mod response;
pub mod resumable;
pub mod scope;
pub mod session;
pub mod token_store;
pub mod upload;

pub use backend::DriveObjectBackend;
pub use client::DriveClient;
pub use http::{HttpRequest, HttpResponse, HttpTransport, Method};
pub use native_http::{NativeHttpTransport, RetryingHttpTransport};
pub use session::{StoredDriveSession, default_token_path, refresh_stored_session};

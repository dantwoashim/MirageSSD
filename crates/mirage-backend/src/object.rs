use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use futures_util::{StreamExt, stream};
use mirage_types::{ByteCount, CheckedRange, ContentHash, MirageError, RepositoryId};
use serde::{Deserialize, Serialize};

use crate::BackendError;

macro_rules! opaque_string_id {
    ($name:ident, $max:expr, $validator:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, MirageError> {
                let value = value.into();
                if value.is_empty() || value.len() > $max || !($validator)(&value) {
                    return Err(MirageError::invalid_argument(concat!(
                        stringify!($name),
                        " is empty, oversized, or contains forbidden characters"
                    )));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = MirageError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

fn valid_backend_id(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte))
}

fn valid_opaque_id(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

opaque_string_id!(BackendId, 64, valid_backend_id);
opaque_string_id!(ProviderObjectId, 1024, valid_opaque_id);
opaque_string_id!(ImmutableRevision, 512, valid_opaque_id);

/// Immutable repository object classes. Mutable hints and leases are deliberately absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    RepositoryConfig,
    Pack,
    Manifest,
    Commit,
    Profile,
}

impl ObjectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepositoryConfig => "repository_config",
            Self::Pack => "pack",
            Self::Manifest => "manifest",
            Self::Commit => "commit",
            Self::Profile => "profile",
        }
    }
}

/// Provider-neutral immutable object identity stored in manifests and commits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteObjectRef {
    pub backend_id: BackendId,
    pub provider_object_id: ProviderObjectId,
    pub immutable_revision: Option<ImmutableRevision>,
    pub byte_length: ByteCount,
    pub content_hash: ContentHash,
    pub kind: ObjectKind,
}

impl RemoteObjectRef {
    pub fn validate(&self) -> Result<(), MirageError> {
        if self.byte_length.is_zero() {
            return Err(MirageError::invalid_argument(
                "immutable remote objects must contain at least one byte",
            ));
        }
        Ok(())
    }
}

/// Provider response facts safe to retain for integrity and diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendResponseMetadata {
    pub provider_request_id: Option<String>,
    pub observed_revision: Option<ImmutableRevision>,
    pub transport_status: Option<u16>,
}

/// Exact immutable-object stat response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStat {
    pub byte_length: ByteCount,
    pub content_hash: ContentHash,
    pub immutable_revision: Option<ImmutableRevision>,
    pub kind: ObjectKind,
}

/// Scheduler priority class, kept independent from every provider API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchClass {
    BlockingRead,
    MandatoryAdmission,
    CapsuleAdmission,
    LiveFrontier,
    ReadAhead,
    IdleWarm,
    Maintenance,
}

/// Explicit evidence required before an immutable object can be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionProof {
    pub repository_id: RepositoryId,
    pub object_hash: ContentHash,
    pub retained_root_set_hash: ContentHash,
    pub validated_at_sequence: u64,
}

/// Stream wrapper that enforces an exact declared byte count.
pub struct BackendByteStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, BackendError>> + Send + 'static>>,
    remaining: u64,
    finished: bool,
}

impl fmt::Debug for BackendByteStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendByteStream")
            .field("remaining", &self.remaining)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl BackendByteStream {
    pub fn new(
        inner: impl Stream<Item = Result<Bytes, BackendError>> + Send + 'static,
        expected_bytes: u64,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            remaining: expected_bytes,
            finished: false,
        }
    }

    #[must_use]
    pub fn from_bytes(bytes: Bytes) -> Self {
        let expected = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Self::new(stream::once(async move { Ok(bytes) }), expected)
    }

    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.remaining
    }

    pub async fn collect_bounded(mut self, maximum: u64) -> Result<Bytes, BackendError> {
        if self.remaining > maximum {
            return Err(BackendError::permanent(
                "byte stream exceeds the caller's collection limit",
            ));
        }
        let capacity = usize::try_from(self.remaining).map_err(|_| {
            BackendError::permanent("byte stream does not fit the process address space")
        })?;
        let mut output = BytesMut::with_capacity(capacity);
        while let Some(chunk) = self.next().await {
            output.extend_from_slice(&chunk?);
        }
        Ok(output.freeze())
    }
}

impl Stream for BackendByteStream {
    type Item = Result<Bytes, BackendError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                let length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                if length == 0 {
                    this.finished = true;
                    return Poll::Ready(Some(Err(BackendError::integrity(
                        "backend produced an empty stream chunk",
                    ))));
                }
                if length > this.remaining {
                    this.finished = true;
                    return Poll::Ready(Some(Err(BackendError::integrity(
                        "backend produced more bytes than declared",
                    ))));
                }
                this.remaining -= length;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) if this.remaining == 0 => {
                this.finished = true;
                Poll::Ready(None)
            }
            Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready(Some(Err(BackendError::integrity(
                    "backend stream ended before its declared length",
                ))))
            }
        }
    }
}

/// Exact range response. Construction rejects a contradictory received length.
#[derive(Debug)]
pub struct BackendRead {
    pub requested_range: CheckedRange,
    pub received_length: ByteCount,
    pub metadata: BackendResponseMetadata,
    pub stream: BackendByteStream,
}

impl BackendRead {
    pub fn new(
        requested_range: CheckedRange,
        received_length: ByteCount,
        metadata: BackendResponseMetadata,
        stream: BackendByteStream,
    ) -> Result<Self, BackendError> {
        if requested_range.len() != received_length.as_u64()
            || stream.remaining() != received_length.as_u64()
        {
            return Err(BackendError::integrity(
                "backend range response length contradicts the request",
            ));
        }
        Ok(Self {
            requested_range,
            received_length,
            metadata,
            stream,
        })
    }

    pub async fn collect_bounded(self, maximum: u64) -> Result<Bytes, BackendError> {
        self.stream.collect_bounded(maximum).await
    }

    #[must_use]
    pub fn into_stream(self) -> BackendByteStream {
        self.stream
    }
}

/// Bounded upload body with an exact declared length.
#[derive(Debug)]
pub struct UploadSource {
    pub length: ByteCount,
    stream: BackendByteStream,
}

impl UploadSource {
    #[must_use]
    pub fn from_bytes(bytes: Bytes) -> Self {
        let length = ByteCount::from_u64(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Self {
            length,
            stream: BackendByteStream::from_bytes(bytes),
        }
    }

    pub fn new(length: ByteCount, stream: BackendByteStream) -> Result<Self, BackendError> {
        if length.as_u64() != stream.remaining() {
            return Err(BackendError::integrity(
                "upload source length contradicts its stream bound",
            ));
        }
        Ok(Self { length, stream })
    }

    pub async fn collect_bounded(self, maximum: u64) -> Result<Bytes, BackendError> {
        self.stream.collect_bounded(maximum).await
    }

    #[must_use]
    pub fn into_stream(self) -> BackendByteStream {
        self.stream
    }
}

//! Retry semantics and dispositions for MirageSSD operations.

use core::time::Duration;
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Retry disposition indicating whether and how a caller or subsystem may retry an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetryDisposition {
    /// The failure is permanent or non-retryable. Do not retry automatically.
    Never,
    /// The failure is transient (e.g. temporary lock contention); retry immediately without delay.
    Immediate,
    /// The failure is transient (e.g. rate limit, network backpressure); retry with exponential backoff.
    Backoff,
    /// The failure has an explicit cooldown; retry after the specified duration.
    After(Duration),
    /// User or operator action is required before retrying (e.g. re-authenticate, free disk space).
    UserAction,
}

#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
struct RetryAfterWire {
    seconds: u64,
    nanoseconds: u32,
}

#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RetryWire {
    Name(String),
    After { after: RetryAfterWire },
}

#[cfg(feature = "serde")]
impl Serialize for RetryDisposition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = match self {
            Self::Never => RetryWire::Name("never".to_string()),
            Self::Immediate => RetryWire::Name("immediate".to_string()),
            Self::Backoff => RetryWire::Name("backoff".to_string()),
            Self::After(duration) => RetryWire::After {
                after: RetryAfterWire {
                    seconds: duration.as_secs(),
                    nanoseconds: duration.subsec_nanos(),
                },
            },
            Self::UserAction => RetryWire::Name("user_action".to_string()),
        };
        wire.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for RetryDisposition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RetryWire::deserialize(deserializer)?;
        match wire {
            RetryWire::Name(name) => match name.as_str() {
                "never" => Ok(Self::Never),
                "immediate" => Ok(Self::Immediate),
                "backoff" => Ok(Self::Backoff),
                "user_action" => Ok(Self::UserAction),
                _ => Err(serde::de::Error::custom("unknown retry disposition")),
            },
            RetryWire::After { after } if after.nanoseconds < 1_000_000_000 => {
                Ok(Self::After(Duration::new(after.seconds, after.nanoseconds)))
            }
            RetryWire::After { .. } => Err(serde::de::Error::custom(
                "retry nanoseconds must be below 1000000000",
            )),
        }
    }
}

impl RetryDisposition {
    /// Returns `true` if the disposition indicates an automated retry (Immediate, Backoff, or After).
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Immediate | Self::Backoff | Self::After(_))
    }

    /// Returns `true` if the disposition is `Never`.
    #[must_use]
    pub const fn is_never(&self) -> bool {
        matches!(self, Self::Never)
    }

    /// Returns `true` if user action is required before retrying.
    #[must_use]
    pub const fn requires_user_action(&self) -> bool {
        matches!(self, Self::UserAction)
    }
}

impl core::fmt::Display for RetryDisposition {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Never => write!(f, "never"),
            Self::Immediate => write!(f, "immediate"),
            Self::Backoff => write!(f, "backoff"),
            Self::After(dur) => write!(f, "after:{}ms", dur.as_millis()),
            Self::UserAction => write!(f, "user_action"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_disposition_predicates() {
        assert!(RetryDisposition::Immediate.is_retryable());
        assert!(RetryDisposition::Backoff.is_retryable());
        assert!(RetryDisposition::After(Duration::from_millis(500)).is_retryable());
        assert!(!RetryDisposition::Never.is_retryable());
        assert!(!RetryDisposition::UserAction.is_retryable());

        assert!(RetryDisposition::Never.is_never());
        assert!(!RetryDisposition::Immediate.is_never());

        assert!(RetryDisposition::UserAction.requires_user_action());
        assert!(!RetryDisposition::Backoff.requires_user_action());
    }

    #[test]
    fn test_retry_disposition_display() {
        assert_eq!(RetryDisposition::Never.to_string(), "never");
        assert_eq!(RetryDisposition::Immediate.to_string(), "immediate");
        assert_eq!(RetryDisposition::Backoff.to_string(), "backoff");
        assert_eq!(
            RetryDisposition::After(Duration::from_millis(250)).to_string(),
            "after:250ms"
        );
        assert_eq!(RetryDisposition::UserAction.to_string(), "user_action");
    }
}

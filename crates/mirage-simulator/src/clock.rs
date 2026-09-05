use mirage_types::MirageError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SimTime(u64);

impl SimTime {
    pub const ZERO: Self = Self(0);
    #[must_use]
    pub const fn from_ns(value: u64) -> Self {
        Self(value)
    }
    #[must_use]
    pub const fn as_ns(self) -> u64 {
        self.0
    }
    pub fn checked_add(self, duration_ns: u64) -> Result<Self, MirageError> {
        self.0
            .checked_add(duration_ns)
            .map(Self)
            .ok_or_else(|| MirageError::invalid_argument("simulated time overflows"))
    }
}

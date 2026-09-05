use mirage_db::Database;
use mirage_types::{MirageError, PageHash, SessionId};

use crate::{PinReason, PinRegistry};

impl PinRegistry {
    pub fn pin_session(
        &self,
        db: &Database,
        session: SessionId,
        pages: &[PageHash],
    ) -> Result<(), MirageError> {
        self.pin_batch(db, PinReason::Session(session), pages)
    }
    pub fn release_session(&self, db: &Database, session: SessionId) -> Result<(), MirageError> {
        self.release(db, PinReason::Session(session))
    }
}

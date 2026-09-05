use std::path::Path;

use mirage_db::check_database;
use mirage_types::MirageError;

use crate::output;

pub fn run(path: &Path, json: bool) -> Result<(), MirageError> {
    let report = check_database(path)?;
    if !report.quick_check_ok
        || !report.integrity_check_ok
        || report.foreign_key_violation_count != 0
    {
        return Err(MirageError::integrity_mismatch(format!(
            "database check failed with {} foreign-key violations",
            report.foreign_key_violation_count
        )));
    }
    if json {
        output::emit_success(&report)
    } else {
        println!("database integrity and foreign keys are valid");
        Ok(())
    }
}

use std::path::Path;

use mirage_config::load_from_path;
use mirage_types::MirageError;
use serde::Serialize;

use crate::output;

#[derive(Debug, Serialize)]
struct ConfigValidation {
    valid: bool,
    format_version: u32,
}

pub fn validate_path(path: &Path, json: bool) -> Result<(), MirageError> {
    let config = load_from_path(path)?;
    let result = ConfigValidation {
        valid: true,
        format_version: config.format_version,
    };
    if json {
        output::emit_success(&result)
    } else {
        println!("configuration valid (format {})", result.format_version);
        Ok(())
    }
}

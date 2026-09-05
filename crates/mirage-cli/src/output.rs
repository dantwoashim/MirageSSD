use std::process::ExitCode;

use mirage_types::{MirageError, MirageErrorKind, PublicErrorEnvelope};
use serde::Serialize;

pub const CLI_ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
struct CliEnvelope<T> {
    envelope_version: u32,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<PublicErrorEnvelope>,
}

pub fn emit_success<T: Serialize>(data: &T) -> Result<(), MirageError> {
    let envelope = CliEnvelope {
        envelope_version: CLI_ENVELOPE_VERSION,
        ok: true,
        data: Some(data),
        error: None,
    };
    let json = serde_json::to_string(&envelope).map_err(|error| {
        MirageError::internal_invariant("CLI response serialization failed").with_source(error)
    })?;
    println!("{json}");
    Ok(())
}

pub fn emit_error(error: &MirageError, json: bool) -> ExitCode {
    if json {
        let envelope: CliEnvelope<()> = CliEnvelope {
            envelope_version: CLI_ENVELOPE_VERSION,
            ok: false,
            data: None,
            error: Some(error.to_public_envelope()),
        };
        match serde_json::to_string(&envelope) {
            Ok(serialized) => eprintln!("{serialized}"),
            Err(_) => eprintln!(
                "{{\"envelope_version\":1,\"ok\":false,\"error\":{{\"code\":\"MIRAGE_INTERNAL_INVARIANT\",\"kind\":\"InternalInvariant\",\"message\":\"CLI response serialization failed\",\"retry\":\"never\"}}}}"
            ),
        }
    } else {
        eprintln!("[{}] {}", error.code, error.message);
    }
    ExitCode::from(exit_code(error.kind))
}

const fn exit_code(kind: MirageErrorKind) -> u8 {
    match kind {
        MirageErrorKind::NotImplemented => 3,
        MirageErrorKind::InvalidArgument | MirageErrorKind::UnsupportedLayout => 4,
        _ => 1,
    }
}

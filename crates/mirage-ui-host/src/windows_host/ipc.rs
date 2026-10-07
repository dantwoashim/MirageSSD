//! Named-pipe IPC helpers for talking to the MirageSSD service.

use super::*;

pub(super) fn inject_drive_access(
    request: &mut Request,
    token_store: Option<&Path>,
) -> Result<(), MirageError> {
    match &request.command {
        Command::CapacityPlan {
            drive_access_token, ..
        }
        | Command::CapacityAcquire {
            drive_access_token, ..
        }
        | Command::NativeActivate {
            drive_access_token, ..
        }
        | Command::Mount {
            drive_access_token, ..
        } if drive_access_token.is_none() => {}
        Command::Materialize {
            drive_access_token,
            drive_quota,
            ..
        } if drive_access_token.is_none() && drive_quota.is_none() => {}
        _ => {
            return Err(MirageError::invalid_argument(
                "the Drive credential bridge only accepts an unauthenticated capacity, materialization, or native activation request",
            ));
        }
    }
    let session =
        futures_executor::block_on(mirage_backend_drive::refresh_stored_session(token_store))?;
    let quota = if matches!(&request.command, Command::Materialize { .. }) {
        Some(
            futures_executor::block_on(mirage_backend_drive::quota::storage_quota(
                session.transport.as_ref(),
                session.access_token.as_str(),
            ))
            .map_err(MirageError::from)?,
        )
    } else {
        None
    };
    let token = SensitiveString::new(session.access_token.as_str().to_owned())?;
    match &mut request.command {
        Command::CapacityPlan {
            drive_access_token, ..
        }
        | Command::CapacityAcquire {
            drive_access_token, ..
        }
        | Command::NativeActivate {
            drive_access_token, ..
        }
        | Command::Mount {
            drive_access_token, ..
        } => *drive_access_token = Some(token),
        Command::Materialize {
            drive_access_token,
            drive_quota,
            ..
        } => {
            *drive_access_token = Some(token);
            let quota = quota.expect("materialization quota was requested");
            *drive_quota = Some(DriveQuotaSnapshot {
                limit_bytes: quota.limit,
                usage_bytes: quota.usage,
            });
        }
        _ => unreachable!("validated Drive bridge command changed"),
    }
    Ok(())
}

pub(super) fn exchange(request: &Request) -> Result<Response, MirageError> {
    let mut pipe = open_pipe()?;
    let frame = encode_frame(request)?;
    pipe.write_all(&frame).map_err(io)?;
    pipe.flush().map_err(io)?;
    let mut prefix = [0_u8; 4];
    pipe.read_exact(&mut prefix).map_err(io)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(MirageError::integrity_mismatch(
            "service response exceeds IPC bound",
        ));
    }
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + length, 0);
    pipe.read_exact(&mut frame[4..]).map_err(io)?;
    let response: Response = decode_frame(&frame)?;
    if response.protocol_version != mirage_ipc::PROTOCOL_VERSION
        || response.request_id != request.request_id
    {
        return Err(MirageError::integrity_mismatch(
            "service response correlation failed",
        ));
    }
    Ok(response)
}

fn open_pipe() -> Result<std::fs::File, MirageError> {
    mirage_cli::client::open_service_pipe_with_retry(mirage_cli::client::SERVICE_PIPE_NAME, 15_000)
}

pub(super) fn io(error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "service IPC failed",
    )
    .with_source(error)
}

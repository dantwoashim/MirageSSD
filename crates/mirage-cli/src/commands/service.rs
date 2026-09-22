use crate::{
    client::{NamedPipeTransport, ServiceTransport},
    output,
};
use mirage_ipc::{Command, PROTOCOL_VERSION, Request, ResponseBody};
use mirage_types::MirageError;
use serde_json::Value;

pub fn run(command: Command, json: bool) -> Result<(), MirageError> {
    run_with(&NamedPipeTransport, command, json)
}
pub fn request_json(command: Command) -> Result<Value, MirageError> {
    request_with(&NamedPipeTransport, command)
}
pub fn run_with(
    transport: &impl ServiceTransport,
    command: Command,
    json: bool,
) -> Result<(), MirageError> {
    emit(request_with(transport, command)?, json)
}

pub fn request_with(
    transport: &dyn ServiceTransport,
    command: Command,
) -> Result<Value, MirageError> {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        cancellation_id: None,
        command,
    };
    let min_protocol = request.command.min_protocol();
    let response = match transport.exchange(&request) {
        Ok(response) => response,
        Err(error)
            if min_protocol > PROTOCOL_VERSION
                && matches!(error.kind, mirage_types::MirageErrorKind::Io) =>
        {
            return Err(MirageError::provider_unavailable(
                "the MirageSSD service is older than this app — restart your PC or reinstall MirageSSD",
            ));
        }
        Err(error) => return Err(error),
    };
    match response.body {
        ResponseBody::Json(value) => Ok(value),
        ResponseBody::Accepted { operation_id } => {
            Ok(serde_json::json!({"accepted":true,"operation_id":operation_id}))
        }
        ResponseBody::Progress { completed, total } => {
            Ok(serde_json::json!({"completed":completed,"total":total}))
        }
        ResponseBody::Error { code, message } => Err(MirageError::repository_conflict(format!(
            "{code}: {message}"
        ))),
    }
}
pub(crate) fn emit(value: Value, json: bool) -> Result<(), MirageError> {
    if json {
        output::emit_success(&value)
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| MirageError::internal_invariant(
                "service output serialization failed"
            )
            .with_source(e))?
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ServiceTransport;
    use mirage_ipc::Response;

    struct DeadTransport;
    impl ServiceTransport for DeadTransport {
        fn exchange(&self, _: &Request) -> Result<Response, MirageError> {
            Err(MirageError::new(
                mirage_types::MirageErrorKind::Io,
                "MIRAGE_IO_ERROR",
                "service IPC failed",
            ))
        }
    }

    #[test]
    fn newer_commands_explain_service_version_skew() {
        let failure = request_with(
            &DeadTransport,
            Command::RepositoryUnregister {
                repository_id: mirage_types::RepositoryId::from_bytes([7; 16]),
                force_unmount: false,
                discard_unpublished: false,
            },
        )
        .unwrap_err();
        assert!(failure.to_string().contains("older than this app"));

        // Old-schema commands keep the underlying transport error.
        let failure = request_with(&DeadTransport, Command::Status).unwrap_err();
        assert!(failure.to_string().contains("service IPC failed"));
    }
}

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

fn request_with(transport: &impl ServiceTransport, command: Command) -> Result<Value, MirageError> {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        cancellation_id: None,
        command,
    };
    let response = transport.exchange(&request)?;
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

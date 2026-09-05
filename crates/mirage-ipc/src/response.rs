use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResponseBody {
    Accepted { operation_id: u64 },
    Progress { completed: u64, total: u64 },
    Json(serde_json::Value),
    Error { code: String, message: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol_version: u16,
    pub request_id: u64,
    pub body: ResponseBody,
}

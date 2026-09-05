use mirage_backend::{
    BackendError, BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef,
};
use mirage_types::{ByteCount, ContentHash, RepositoryId};
use serde::Deserialize;

use crate::http::{HttpRequest, HttpTransport, Method};

#[derive(Deserialize)]
struct List {
    files: Vec<File>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    id: String,
    size: String,
    head_revision_id: Option<String>,
}

pub async fn find_exact(
    transport: &dyn HttpTransport,
    access_token: &str,
    repository: RepositoryId,
    kind: ObjectKind,
    hash: ContentHash,
    length: u64,
) -> Result<Option<RemoteObjectRef>, BackendError> {
    let query = format!(
        "trashed = false and appProperties has {{ key='mirage_repository' and value='{repository}' }} and appProperties has {{ key='mirage_kind' and value='{}' }} and appProperties has {{ key='mirage_hash' and value='{hash}' }}",
        kind.as_str()
    );
    let mut url = url::Url::parse("https://www.googleapis.com/drive/v3/files")
        .map_err(|_| BackendError::permanent("Drive lookup URL is invalid"))?;
    url.query_pairs_mut()
        .append_pair("q", &query)
        .append_pair("spaces", "drive")
        .append_pair("pageSize", "100")
        .append_pair("fields", "files(id,size,headRevisionId)");
    let response = transport
        .execute(HttpRequest {
            method: Method::Get,
            url: url.into(),
            headers: [("authorization".into(), format!("Bearer {access_token}"))].into(),
            body: bytes::Bytes::new(),
        })
        .await?;
    if response.status != 200 {
        return Err(crate::error::classify_response(&response));
    }
    let mut files: List = serde_json::from_slice(&response.body)
        .map_err(|_| BackendError::integrity("Drive object lookup was invalid"))?;
    if files
        .files
        .iter()
        .any(|file| file.size.parse::<u64>().ok() != Some(length))
    {
        return Err(BackendError::integrity(
            "Drive contains conflicting object identity",
        ));
    }
    files.files.sort_by(|a, b| a.id.cmp(&b.id));
    let Some(file) = files.files.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(RemoteObjectRef {
        backend_id: BackendId::new("drive")
            .map_err(|_| BackendError::permanent("Drive backend ID is invalid"))?,
        provider_object_id: ProviderObjectId::new(file.id)
            .map_err(|_| BackendError::integrity("Drive lookup returned invalid file ID"))?,
        immutable_revision: file
            .head_revision_id
            .map(ImmutableRevision::new)
            .transpose()
            .map_err(|_| BackendError::integrity("Drive lookup returned invalid revision"))?,
        byte_length: ByteCount::from_u64(length),
        content_hash: hash,
        kind,
    }))
}

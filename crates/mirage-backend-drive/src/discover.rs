use std::collections::BTreeMap;

use mirage_backend::{
    BackendError, BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef,
};
use mirage_types::{ByteCount, ContentHash, RepositoryId};
use serde::Deserialize;

use crate::http::{HttpRequest, HttpTransport, Method};

const PAGE_LIMIT: usize = 10_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    files: Vec<ListedFile>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedFile {
    id: String,
    size: String,
    head_revision_id: Option<String>,
    app_properties: BTreeMap<String, String>,
}

pub async fn enumerate_commits(
    transport: &dyn HttpTransport,
    access_token: &str,
    repository: RepositoryId,
) -> Result<Vec<RemoteObjectRef>, BackendError> {
    let repository_text = repository.to_string();
    let query = format!(
        "trashed = false and appProperties has {{ key='mirage_repository' and value='{repository_text}' }} and appProperties has {{ key='mirage_kind' and value='commit' }}"
    );
    let backend_id = BackendId::new("drive")
        .map_err(|_| BackendError::permanent("Drive backend ID is invalid"))?;
    let mut page_token: Option<String> = None;
    let mut objects = Vec::new();
    for _ in 0..PAGE_LIMIT {
        let mut url = url::Url::parse("https://www.googleapis.com/drive/v3/files")
            .map_err(|_| BackendError::permanent("Drive list URL is invalid"))?;
        url.query_pairs_mut()
            .append_pair("q", &query)
            .append_pair("spaces", "drive")
            .append_pair("pageSize", "1000")
            .append_pair(
                "fields",
                "nextPageToken,files(id,size,headRevisionId,appProperties)",
            );
        if let Some(token) = &page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
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
        let list: FileList = serde_json::from_slice(&response.body)
            .map_err(|_| BackendError::integrity("Drive commit listing was invalid"))?;
        for file in list.files {
            objects.push(parse_commit(file, &backend_id, &repository_text)?);
        }
        match list.next_page_token {
            Some(token) if !token.is_empty() => page_token = Some(token),
            _ => {
                objects.sort_by(|a, b| {
                    a.provider_object_id
                        .as_str()
                        .cmp(b.provider_object_id.as_str())
                });
                return Ok(objects);
            }
        }
    }
    Err(BackendError::permanent(
        "Drive commit pagination exceeded safety bound",
    ))
}

fn parse_commit(
    file: ListedFile,
    backend_id: &BackendId,
    repository: &str,
) -> Result<RemoteObjectRef, BackendError> {
    if file
        .app_properties
        .get("mirage_repository")
        .map(String::as_str)
        != Some(repository)
        || file.app_properties.get("mirage_kind").map(String::as_str) != Some("commit")
    {
        return Err(BackendError::integrity(
            "Drive returned commit with contradictory metadata",
        ));
    }
    let hash = file
        .app_properties
        .get("mirage_hash")
        .ok_or_else(|| BackendError::integrity("Drive commit metadata omitted hash"))?
        .parse::<ContentHash>()
        .map_err(|_| BackendError::integrity("Drive commit metadata contained invalid hash"))?;
    let size = file
        .size
        .parse::<u64>()
        .map_err(|_| BackendError::integrity("Drive commit metadata contained invalid size"))?;
    if size == 0 {
        return Err(BackendError::integrity("Drive commit object is empty"));
    }
    Ok(RemoteObjectRef {
        backend_id: backend_id.clone(),
        provider_object_id: ProviderObjectId::new(file.id)
            .map_err(|_| BackendError::integrity("Drive returned invalid file ID"))?,
        immutable_revision: file
            .head_revision_id
            .map(ImmutableRevision::new)
            .transpose()
            .map_err(|_| BackendError::integrity("Drive returned invalid revision"))?,
        byte_length: ByteCount::from_u64(size),
        content_hash: hash,
        kind: ObjectKind::Commit,
    })
}

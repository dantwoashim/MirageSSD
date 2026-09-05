use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{
    BackendError, BackendErrorClass, BackendId, BackendRead, DeletionProof, FetchClass,
    ObjectBackend, ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};
use mirage_types::{BackendHealthState, ByteCount, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::http::{HttpRequest, HttpTransport, Method};

pub struct DriveObjectBackend {
    transport: Arc<dyn HttpTransport>,
    token: Zeroizing<String>,
    repository: RepositoryId,
    backend_id: BackendId,
}

impl DriveObjectBackend {
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        token: Zeroizing<String>,
        repository: RepositoryId,
    ) -> Result<Self, BackendError> {
        Ok(Self {
            transport,
            token,
            repository,
            backend_id: BackendId::new("drive")
                .map_err(|_| BackendError::permanent("Drive backend ID is invalid"))?,
        })
    }
    fn validate_ref(&self, object: &RemoteObjectRef) -> Result<(), BackendError> {
        if object.backend_id != self.backend_id {
            return Err(BackendError::permanent("object belongs to another backend"));
        }
        object
            .validate()
            .map_err(|_| BackendError::integrity("Drive object reference is invalid"))
    }
    fn auth(&self) -> String {
        format!("Bearer {}", self.token.as_str())
    }
}
impl std::fmt::Debug for DriveObjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveObjectBackend")
            .field("repository", &self.repository)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ObjectBackend for DriveObjectBackend {
    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        _class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        self.validate_ref(object)?;
        crate::read::read_exact(self.transport.as_ref(), &self.token, object, range, &cancel).await
    }
    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        if cancel.is_cancelled() {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "Drive upload was cancelled",
            ));
        }
        let length = source.length.as_u64();
        let bytes = source.collect_bounded(length).await?;
        if blake3::hash(&bytes).as_bytes() != expected_hash.as_bytes() {
            return Err(BackendError::integrity("Drive upload source hash mismatch"));
        }
        if kind == ObjectKind::Pack {
            require_encrypted_pack(&bytes)?;
        }
        if let Some(existing) = crate::lookup::find_exact(
            self.transport.as_ref(),
            &self.token,
            self.repository,
            kind,
            expected_hash,
            length,
        )
        .await?
        {
            return Ok(existing);
        }
        let mut properties = BTreeMap::new();
        properties.insert("mirage_repository".into(), self.repository.to_string());
        properties.insert("mirage_kind".into(), kind.as_str().into());
        properties.insert("mirage_hash".into(), expected_hash.to_string());
        let name = format!("{}-{expected_hash}.bin", kind.as_str());
        let completed = crate::upload::upload_bytes(
            self.transport.as_ref(),
            &self.token,
            &name,
            &properties,
            bytes,
        )
        .await?;
        if cancel.is_cancelled() {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "Drive upload was cancelled after completion",
            ));
        }
        Ok(RemoteObjectRef {
            backend_id: self.backend_id.clone(),
            provider_object_id: completed.file_id,
            immutable_revision: completed.revision,
            byte_length: ByteCount::from_u64(length),
            content_hash: expected_hash,
            kind,
        })
    }
    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        self.validate_ref(object)?;
        let url = format!(
            "https://www.googleapis.com/drive/v3/files/{}?fields=id,size,headRevisionId,appProperties",
            object.provider_object_id.as_str()
        );
        let response = self
            .transport
            .execute(HttpRequest {
                method: Method::Get,
                url,
                headers: [("authorization".into(), self.auth())].into(),
                body: Bytes::new(),
            })
            .await?;
        if response.status != 200 {
            return Err(crate::error::classify_response(&response));
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Stat {
            size: String,
            head_revision_id: Option<String>,
            app_properties: BTreeMap<String, String>,
        }
        let stat: Stat = serde_json::from_slice(&response.body)
            .map_err(|_| BackendError::integrity("Drive stat metadata was invalid"))?;
        let size = stat
            .size
            .parse::<u64>()
            .map_err(|_| BackendError::integrity("Drive stat size was invalid"))?;
        let hash = stat
            .app_properties
            .get("mirage_hash")
            .ok_or_else(|| BackendError::integrity("Drive stat omitted content hash"))?
            .parse::<ContentHash>()
            .map_err(|_| BackendError::integrity("Drive stat hash was invalid"))?;
        if size != object.byte_length.as_u64()
            || hash != object.content_hash
            || stat
                .app_properties
                .get("mirage_repository")
                .map(String::as_str)
                != Some(self.repository.to_string().as_str())
            || stat.app_properties.get("mirage_kind").map(String::as_str)
                != Some(object.kind.as_str())
        {
            return Err(BackendError::integrity(
                "Drive immutable object identity changed",
            ));
        }
        let revision = stat
            .head_revision_id
            .map(mirage_backend::ImmutableRevision::new)
            .transpose()
            .map_err(|_| BackendError::integrity("Drive stat revision was invalid"))?;
        if object.immutable_revision.is_some() && revision != object.immutable_revision {
            return Err(BackendError::integrity(
                "Drive immutable object revision changed",
            ));
        }
        if object.kind == ObjectKind::Pack {
            let header = crate::read::read_exact(
                self.transport.as_ref(),
                &self.token,
                object,
                CheckedRange::new(0, mirage_pack::format::PACK_HEADER_LEN as u64)
                    .map_err(|_| BackendError::integrity("Drive pack header range is invalid"))?,
                &CancellationToken::new(),
            )
            .await?
            .collect_bounded(mirage_pack::format::PACK_HEADER_LEN as u64)
            .await?;
            require_encrypted_pack(&header)?;
        }
        Ok(ObjectStat {
            byte_length: ByteCount::from_u64(size),
            content_hash: hash,
            immutable_revision: revision,
            kind: object.kind,
        })
    }
    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        if repository != self.repository {
            return Ok(Vec::new());
        }
        crate::discover::enumerate_commits(self.transport.as_ref(), &self.token, repository).await
    }
    async fn delete_immutable(
        &self,
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        self.validate_ref(object)?;
        if proof.repository_id != self.repository || proof.object_hash != object.content_hash {
            return Err(BackendError::new(
                BackendErrorClass::Permission,
                "deletion proof does not authorize Drive object",
            ));
        }
        if cancel.is_cancelled() {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "Drive deletion was cancelled",
            ));
        }
        let url = format!(
            "https://www.googleapis.com/drive/v3/files/{}",
            object.provider_object_id.as_str()
        );
        let response = self
            .transport
            .execute(HttpRequest {
                method: Method::Delete,
                url,
                headers: [("authorization".into(), self.auth())].into(),
                body: Bytes::new(),
            })
            .await?;
        if response.status == 204 || response.status == 404 {
            Ok(())
        } else {
            Err(crate::error::classify_response(&response))
        }
    }
    async fn health(&self) -> BackendHealthState {
        BackendHealthState::Unknown
    }
}

fn require_encrypted_pack(bytes: &[u8]) -> Result<(), BackendError> {
    let header = bytes
        .get(..mirage_pack::format::PACK_HEADER_LEN)
        .ok_or_else(|| BackendError::integrity("Drive pack header is truncated"))?;
    let header = mirage_pack::format::PackHeader::decode(header)
        .map_err(|_| BackendError::integrity("Drive pack header is invalid"))?;
    if !header.encrypted {
        return Err(BackendError::new(
            BackendErrorClass::Permission,
            "Drive publication requires client-side encrypted packs",
        ));
    }
    Ok(())
}

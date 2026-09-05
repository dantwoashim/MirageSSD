use std::fmt;
use std::path::{Path, PathBuf};

use base64::Engine;
use mirage_crypto::dpapi::{self, ProtectionScope};
use mirage_types::MirageError;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const FORMAT_VERSION: u16 = 2;
const DEVICE_FORMAT_VERSION: u16 = 3;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredToken {
    format_version: u16,
    #[serde(default)]
    client_id: Option<String>,
    account_id: String,
    scopes: Vec<String>,
    issued_unix_seconds: u64,
    ciphertext_base64: String,
    #[serde(default)]
    client_secret_ciphertext_base64: Option<String>,
}
impl fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredToken")
            .field("format_version", &self.format_version)
            .field("account_id", &self.account_id)
            .field("scopes", &self.scopes)
            .field("issued_unix_seconds", &self.issued_unix_seconds)
            .field("ciphertext_base64", &"[REDACTED]")
            .field(
                "client_secret_ciphertext_base64",
                &self
                    .client_secret_ciphertext_base64
                    .as_ref()
                    .map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMetadata {
    pub client_id: Option<String>,
    pub account_id: String,
    pub scopes: Vec<String>,
    pub issued_unix_seconds: u64,
}
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}
impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn save(
        &self,
        client_id: &str,
        account_id: &str,
        scopes: &[String],
        issued_unix_seconds: u64,
        refresh_token: &mut Zeroizing<Vec<u8>>,
    ) -> Result<(), MirageError> {
        validate_client_id(client_id)?;
        validate_metadata(account_id, scopes)?;
        let encrypted = dpapi::protect(
            refresh_token,
            &entropy(account_id),
            ProtectionScope::CurrentUser,
        )?;
        refresh_token.zeroize();
        let record = StoredToken {
            format_version: FORMAT_VERSION,
            client_id: Some(client_id.to_owned()),
            account_id: account_id.to_owned(),
            scopes: scopes.to_vec(),
            issued_unix_seconds,
            ciphertext_base64: base64::engine::general_purpose::STANDARD.encode(encrypted),
            client_secret_ciphertext_base64: None,
        };
        self.write_record(&record)
    }
    pub fn load(&self) -> Result<(TokenMetadata, Zeroizing<Vec<u8>>), MirageError> {
        let record = self.read_record()?;
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(&record.ciphertext_base64)
            .map_err(|_| MirageError::manifest_invalid("token ciphertext is invalid"))?;
        let token = dpapi::unprotect(&ciphertext, &entropy(&record.account_id))?;
        Ok((
            TokenMetadata {
                client_id: record.client_id,
                account_id: record.account_id,
                scopes: record.scopes,
                issued_unix_seconds: record.issued_unix_seconds,
            },
            token,
        ))
    }
    pub fn metadata(&self) -> Result<TokenMetadata, MirageError> {
        let record = self.read_record()?;
        Ok(TokenMetadata {
            client_id: record.client_id,
            account_id: record.account_id,
            scopes: record.scopes,
            issued_unix_seconds: record.issued_unix_seconds,
        })
    }
    pub fn delete(&self) -> Result<bool, MirageError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(MirageError::from(error)),
        }
    }
    pub fn bind_client_id(&self, client_id: &str) -> Result<(), MirageError> {
        validate_client_id(client_id)?;
        let mut record = self.read_record()?;
        if record
            .client_id
            .as_deref()
            .is_some_and(|stored| stored != client_id)
        {
            return Err(MirageError::repository_conflict(
                "stored Drive token belongs to a different OAuth client",
            ));
        }
        if record.client_id.as_deref() == Some(client_id)
            && matches!(
                record.format_version,
                FORMAT_VERSION | DEVICE_FORMAT_VERSION
            )
        {
            return Ok(());
        }
        record.format_version = FORMAT_VERSION;
        record.client_id = Some(client_id.to_owned());
        self.write_record(&record)
    }
    pub fn bind_client_secret(
        &self,
        client_id: &str,
        client_secret: &mut Zeroizing<Vec<u8>>,
    ) -> Result<(), MirageError> {
        validate_client_id(client_id)?;
        if client_secret.is_empty()
            || client_secret.len() > 512
            || client_secret.iter().any(|byte| byte.is_ascii_control())
        {
            return Err(MirageError::invalid_argument(
                "invalid Google OAuth desktop client credential",
            ));
        }
        let mut record = self.read_record()?;
        if record.client_id.as_deref() != Some(client_id) {
            return Err(MirageError::repository_conflict(
                "Drive client credential does not match the protected login",
            ));
        }
        let encrypted = dpapi::protect(
            client_secret,
            &client_secret_entropy(&record.account_id),
            ProtectionScope::CurrentUser,
        )?;
        client_secret.zeroize();
        record.format_version = DEVICE_FORMAT_VERSION;
        record.client_secret_ciphertext_base64 =
            Some(base64::engine::general_purpose::STANDARD.encode(encrypted));
        self.write_record(&record)
    }
    pub fn load_client_secret(&self) -> Result<Zeroizing<Vec<u8>>, MirageError> {
        self.load_client_secret_optional()?.ok_or_else(|| {
            MirageError::backend_unauthenticated("persistent device mounting is not authorized")
        })
    }
    pub fn load_client_secret_optional(&self) -> Result<Option<Zeroizing<Vec<u8>>>, MirageError> {
        let record = self.read_record()?;
        let Some(ciphertext) = record.client_secret_ciphertext_base64.as_deref() else {
            return Ok(None);
        };
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|_| {
                MirageError::manifest_invalid("client credential ciphertext is invalid")
            })?;
        dpapi::unprotect(&ciphertext, &client_secret_entropy(&record.account_id)).map(Some)
    }
    fn read_record(&self) -> Result<StoredToken, MirageError> {
        let metadata = std::fs::metadata(&self.path).map_err(MirageError::from)?;
        if metadata.len() > MAX_RECORD_BYTES {
            return Err(MirageError::manifest_invalid("token record exceeds bound"));
        }
        let bytes = std::fs::read(&self.path).map_err(MirageError::from)?;
        let record: StoredToken = serde_json::from_slice(&bytes)
            .map_err(|_| MirageError::manifest_invalid("token record is invalid"))?;
        if !matches!(
            record.format_version,
            1 | FORMAT_VERSION | DEVICE_FORMAT_VERSION
        ) {
            return Err(MirageError::unsupported_layout(
                "unsupported token record version",
            ));
        }
        if matches!(
            record.format_version,
            FORMAT_VERSION | DEVICE_FORMAT_VERSION
        ) {
            validate_client_id(record.client_id.as_deref().ok_or_else(|| {
                MirageError::manifest_invalid("token record omits its OAuth client ID")
            })?)?;
        } else if record.client_id.is_some() || record.client_secret_ciphertext_base64.is_some() {
            return Err(MirageError::manifest_invalid(
                "legacy token record contains unexpected client metadata",
            ));
        }
        if record.format_version == FORMAT_VERSION
            && record.client_secret_ciphertext_base64.is_some()
        {
            return Err(MirageError::manifest_invalid(
                "token-only credential record contains a device client credential",
            ));
        }
        if record.format_version == DEVICE_FORMAT_VERSION
            && record.client_secret_ciphertext_base64.is_none()
        {
            return Err(MirageError::manifest_invalid(
                "device credential record omits its encrypted client credential",
            ));
        }
        validate_metadata(&record.account_id, &record.scopes)?;
        Ok(record)
    }
    fn write_record(&self, record: &StoredToken) -> Result<(), MirageError> {
        let bytes = serde_json::to_vec(record)
            .map_err(|_| MirageError::internal_invariant("token record serialization failed"))?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(MirageError::invalid_argument("token record exceeds bound"));
        }
        mirage_crypto::durable_file::write_atomic(&self.path, &bytes)
    }
}
fn entropy(account_id: &str) -> Vec<u8> {
    let mut value = b"miragessd/drive/oauth/v1\0".to_vec();
    value.extend_from_slice(account_id.as_bytes());
    value
}
fn client_secret_entropy(account_id: &str) -> Vec<u8> {
    let mut value = b"miragessd/drive/oauth-client/v1\0".to_vec();
    value.extend_from_slice(account_id.as_bytes());
    value
}
fn validate_metadata(account_id: &str, scopes: &[String]) -> Result<(), MirageError> {
    if account_id.is_empty() || account_id.len() > 320 || account_id.chars().any(char::is_control) {
        return Err(MirageError::invalid_argument("invalid account id"));
    }
    if scopes.is_empty()
        || scopes.len() > 16
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 256
                || !scope.starts_with("https://www.googleapis.com/auth/")
        })
    {
        return Err(MirageError::invalid_argument("invalid OAuth scopes"));
    }
    Ok(())
}

fn validate_client_id(client_id: &str) -> Result<(), MirageError> {
    if client_id.is_empty()
        || client_id.len() > 512
        || client_id.chars().any(char::is_control)
        || !client_id.ends_with(".apps.googleusercontent.com")
    {
        return Err(MirageError::invalid_argument(
            "invalid Google OAuth desktop client ID",
        ));
    }
    Ok(())
}

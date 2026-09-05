use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mirage_backend_drive::NativeHttpTransport;
use mirage_backend_drive::oauth::{OAuthAttempt, account_id, exchange_code};
use mirage_backend_drive::scope::DRIVE_FILE;
use mirage_backend_drive::token_store::TokenStore;
use mirage_types::MirageError;
use serde::Serialize;
use url::Url;
use zeroize::Zeroizing;

use crate::output;

const CLIENT_ID_ENV: &str = "MIRAGE_DRIVE_CLIENT_ID";
const CLIENT_CREDENTIALS_ENV: &str = "MIRAGE_DRIVE_CLIENT_CREDENTIALS";
const MAX_CLIENT_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(serde::Deserialize)]
struct GoogleClientFile {
    installed: GoogleInstalledClient,
}

#[derive(serde::Deserialize)]
struct GoogleInstalledClient {
    client_id: String,
    client_secret: String,
}

struct ClientCredentials {
    client_id: String,
    client_secret: Option<Zeroizing<String>>,
}

#[derive(Debug, Serialize)]
struct BackendStatus {
    provider: &'static str,
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issued_unix_seconds: Option<u64>,
    credential_protection: &'static str,
}

pub fn login(
    client_id: Option<&str>,
    client_credentials: Option<&Path>,
    token_path: Option<&Path>,
    timeout_seconds: u64,
    json: bool,
) -> Result<(), MirageError> {
    if !(30..=900).contains(&timeout_seconds) {
        return Err(MirageError::invalid_argument(
            "OAuth timeout must be between 30 and 900 seconds",
        ));
    }
    let credentials = resolve_client_credentials(client_id, client_credentials)?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(MirageError::from)?;
    listener.set_nonblocking(true).map_err(MirageError::from)?;
    let port = listener.local_addr().map_err(MirageError::from)?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let attempt = OAuthAttempt::new().map_err(MirageError::from)?;
    let authorization_url = attempt
        .authorization_url(&credentials.client_id, &redirect_uri)
        .map_err(MirageError::from)?;

    if !json {
        println!("Opening Google authorization in your default browser...");
        println!("Waiting up to {timeout_seconds} seconds for the loopback callback.");
    }
    open_browser(&authorization_url)?;
    let code = wait_for_callback(&listener, &attempt, Duration::from_secs(timeout_seconds))?;

    let transport = NativeHttpTransport::new().map_err(MirageError::from)?;
    let mut grant = futures_executor::block_on(exchange_code(
        &transport,
        &credentials.client_id,
        credentials.client_secret.as_deref().map(String::as_str),
        &code,
        attempt.verifier(),
        &redirect_uri,
    ))
    .map_err(MirageError::from)?;
    if grant.scopes.is_empty() {
        grant.scopes.push(DRIVE_FILE.to_owned());
    }
    if grant.scopes.len() != 1 || grant.scopes[0] != DRIVE_FILE {
        return Err(MirageError::backend_permission_denied(
            "Google returned an unexpected OAuth scope set",
        ));
    }
    let account = futures_executor::block_on(account_id(&transport, &grant.access_token))
        .map_err(MirageError::from)?;
    let refresh = grant.refresh_token.take().ok_or_else(|| {
        MirageError::backend_unauthenticated("Google did not issue an offline refresh token")
    })?;
    let mut refresh_bytes = Zeroizing::new(refresh.as_bytes().to_vec());
    let issued = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MirageError::internal_invariant("system clock precedes Unix epoch"))?
        .as_secs();
    let store = TokenStore::new(resolve_token_path(token_path)?);
    store.save(
        &credentials.client_id,
        &account,
        &grant.scopes,
        issued,
        &mut refresh_bytes,
    )?;
    emit(
        BackendStatus {
            provider: "google-drive",
            authenticated: true,
            account_id: Some(account),
            scopes: grant.scopes,
            issued_unix_seconds: Some(issued),
            credential_protection: "current-user-dpapi",
        },
        json,
    )
}

fn resolve_client_credentials(
    explicit_id: Option<&str>,
    explicit_file: Option<&Path>,
) -> Result<ClientCredentials, MirageError> {
    let file = explicit_file
        .map(Path::to_owned)
        .or_else(|| std::env::var_os(CLIENT_CREDENTIALS_ENV).map(PathBuf::from));
    if let Some(path) = file {
        if !path.is_absolute() {
            return Err(MirageError::invalid_argument(
                "OAuth client credential path must be absolute",
            ));
        }
        let metadata = std::fs::metadata(&path).map_err(MirageError::from)?;
        if metadata.len() == 0 || metadata.len() > MAX_CLIENT_CREDENTIAL_BYTES {
            return Err(MirageError::invalid_argument(
                "OAuth client credential file size is invalid",
            ));
        }
        let bytes = std::fs::read(&path).map_err(MirageError::from)?;
        let parsed: GoogleClientFile = serde_json::from_slice(&bytes)
            .map_err(|_| MirageError::invalid_argument("OAuth Desktop client JSON is invalid"))?;
        validate_client_id(&parsed.installed.client_id)?;
        if parsed.installed.client_secret.is_empty()
            || parsed.installed.client_secret.len() > 512
            || parsed.installed.client_secret.chars().any(char::is_control)
        {
            return Err(MirageError::invalid_argument(
                "OAuth Desktop client secret is invalid",
            ));
        }
        if explicit_id.is_some_and(|value| value != parsed.installed.client_id) {
            return Err(MirageError::invalid_argument(
                "OAuth client ID does not match the Desktop client JSON",
            ));
        }
        return Ok(ClientCredentials {
            client_id: parsed.installed.client_id,
            client_secret: Some(Zeroizing::new(parsed.installed.client_secret)),
        });
    }

    let client_id = explicit_id
        .map(str::to_owned)
        .or_else(|| std::env::var(CLIENT_ID_ENV).ok())
        .ok_or_else(|| {
            MirageError::invalid_argument(format!(
                "Google OAuth client ID is required via --client-id, --client-credentials, or {CLIENT_ID_ENV}"
            ))
        })?;
    validate_client_id(&client_id)?;
    Ok(ClientCredentials {
        client_id,
        client_secret: None,
    })
}

pub fn status(token_path: Option<&Path>, json: bool) -> Result<(), MirageError> {
    let store = TokenStore::new(resolve_token_path(token_path)?);
    if !store.path().exists() {
        return emit(
            BackendStatus {
                provider: "google-drive",
                authenticated: false,
                account_id: None,
                scopes: Vec::new(),
                issued_unix_seconds: None,
                credential_protection: "current-user-dpapi",
            },
            json,
        );
    }
    let metadata = store.metadata()?;
    emit(
        BackendStatus {
            provider: "google-drive",
            authenticated: true,
            account_id: Some(metadata.account_id),
            scopes: metadata.scopes,
            issued_unix_seconds: Some(metadata.issued_unix_seconds),
            credential_protection: "current-user-dpapi",
        },
        json,
    )
}

pub fn logout(token_path: Option<&Path>, json: bool) -> Result<(), MirageError> {
    let deleted = TokenStore::new(resolve_token_path(token_path)?).delete()?;
    if json {
        output::emit_success(&serde_json::json!({
            "provider": "google-drive",
            "authenticated": false,
            "credential_removed": deleted
        }))
    } else {
        println!(
            "Google Drive credentials {}.",
            if deleted {
                "removed"
            } else {
                "were not present"
            }
        );
        Ok(())
    }
}

fn emit(status: BackendStatus, json: bool) -> Result<(), MirageError> {
    if json {
        output::emit_success(&status)
    } else {
        println!(
            "Google Drive: {}",
            if status.authenticated {
                "authenticated"
            } else {
                "not authenticated"
            }
        );
        if let Some(account) = status.account_id {
            println!("Account ID: {account}");
        }
        if !status.scopes.is_empty() {
            println!("Scopes: {}", status.scopes.join(", "));
        }
        println!("Credential protection: {}", status.credential_protection);
        Ok(())
    }
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

fn resolve_token_path(explicit: Option<&Path>) -> Result<PathBuf, MirageError> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(MirageError::invalid_argument(
                "token store path must be absolute",
            ));
        }
        return Ok(path.to_owned());
    }
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::invalid_argument("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local_app_data)
        .join("MirageSSD")
        .join("credentials")
        .join("drive-token.json"))
}

#[cfg(windows)]
fn open_browser(url: &Url) -> Result<(), MirageError> {
    let status = std::process::Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(url.as_str())
        .status()
        .map_err(MirageError::from)?;
    if !status.success() {
        return Err(MirageError::backend_unavailable(
            "default browser could not be opened",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_browser(_url: &Url) -> Result<(), MirageError> {
    Err(MirageError::provider_unavailable(
        "interactive Drive authentication requires Windows",
    ))
}

fn wait_for_callback(
    listener: &TcpListener,
    attempt: &OAuthAttempt,
    timeout: Duration,
) -> Result<String, MirageError> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => match parse_stream(&mut stream, attempt) {
                Ok(Some(code)) => {
                    respond(
                        &mut stream,
                        200,
                        "Authorization complete. You may close this tab.",
                    );
                    return Ok(code);
                }
                Ok(None) => respond(&mut stream, 404, "Not found."),
                Err(error) => {
                    respond(
                        &mut stream,
                        400,
                        "Authorization failed. Return to MirageSSD.",
                    );
                    return Err(error);
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(MirageError::backend_unavailable("OAuth callback timed out"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(MirageError::from(error)),
        }
    }
}

fn parse_stream(
    stream: &mut TcpStream,
    attempt: &OAuthAttempt,
) -> Result<Option<String>, MirageError> {
    stream.set_nonblocking(false).map_err(MirageError::from)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(MirageError::from)?;
    let mut bytes = [0_u8; 16 * 1024];
    let count = stream.read(&mut bytes).map_err(MirageError::from)?;
    let request = std::str::from_utf8(&bytes[..count])
        .map_err(|_| MirageError::invalid_argument("OAuth callback request was invalid"))?;
    let target = request
        .lines()
        .next()
        .and_then(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next(), parts.next()) {
                (Some("GET"), Some(target), Some("HTTP/1.1" | "HTTP/1.0")) => Some(target),
                _ => None,
            }
        })
        .ok_or_else(|| MirageError::invalid_argument("OAuth callback request was invalid"))?;
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| MirageError::invalid_argument("OAuth callback URL was invalid"))?;
    if url.path() != "/oauth/callback" {
        return Ok(None);
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" if state.is_none() => state = Some(value.into_owned()),
            "code" if code.is_none() => code = Some(value.into_owned()),
            "error" if error.is_none() => error = Some(value.into_owned()),
            _ => {}
        }
    }
    let state = state
        .ok_or_else(|| MirageError::backend_unauthenticated("OAuth callback omitted state"))?;
    attempt
        .validate_callback(&state, code.as_deref(), error.as_deref())
        .map(Some)
        .map_err(MirageError::from)
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body =
        format!("<!doctype html><meta charset=utf-8><title>MirageSSD</title><p>{message}</p>");
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

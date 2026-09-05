use base64::Engine;
use mirage_backend::{BackendError, BackendErrorClass};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

use crate::http::{HttpRequest, HttpTransport, Method};

#[derive(Debug, Clone)]
pub struct OAuthAttempt {
    state: String,
    verifier: Zeroizing<String>,
    challenge: String,
}

impl OAuthAttempt {
    pub fn new() -> Result<Self, BackendError> {
        let mut state_bytes = [0_u8; 32];
        let mut verifier_bytes = [0_u8; 32];
        getrandom::fill(&mut state_bytes)
            .map_err(|_| BackendError::permanent("secure randomness unavailable"))?;
        getrandom::fill(&mut verifier_bytes)
            .map_err(|_| BackendError::permanent("secure randomness unavailable"))?;
        let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);
        let verifier =
            Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes));
        state_bytes.zeroize();
        verifier_bytes.zeroize();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Ok(Self {
            state,
            verifier,
            challenge,
        })
    }
    pub fn authorization_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
    ) -> Result<Url, BackendError> {
        let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
            .map_err(|_| BackendError::permanent("OAuth authorization URL is invalid"))?;
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", crate::scope::DRIVE_FILE)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &self.state)
            .append_pair("code_challenge", &self.challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url)
    }
    pub fn validate_callback(
        &self,
        returned_state: &str,
        code: Option<&str>,
        error: Option<&str>,
    ) -> Result<String, BackendError> {
        if returned_state.as_bytes() != self.state.as_bytes() {
            return Err(BackendError::new(
                BackendErrorClass::Authentication,
                "OAuth state mismatch",
            ));
        }
        if error.is_some() {
            return Err(BackendError::new(
                BackendErrorClass::Authentication,
                "OAuth consent was denied",
            ));
        }
        code.filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorClass::Authentication,
                    "OAuth callback omitted authorization code",
                )
            })
    }
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

#[derive(Debug, Deserialize)]
struct TokenWire {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct TokenErrorWire {
    error: String,
    error_description: Option<String>,
}

pub struct TokenGrant {
    pub access_token: Zeroizing<String>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub expires_in_seconds: u64,
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DriveAbout {
    user: DriveUser,
}

#[derive(Debug, Deserialize)]
struct DriveUser {
    #[serde(rename = "permissionId")]
    permission_id: String,
}
impl std::fmt::Debug for TokenGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenGrant")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in_seconds", &self.expires_in_seconds)
            .field("scopes", &self.scopes)
            .finish()
    }
}

pub async fn exchange_code(
    transport: &dyn HttpTransport,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenGrant, BackendError> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("client_id", client_id);
    if let Some(client_secret) = client_secret {
        form.append_pair("client_secret", client_secret);
    }
    let body = form
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("grant_type", "authorization_code")
        .append_pair("redirect_uri", redirect_uri)
        .finish()
        .into_bytes();
    let response = transport
        .execute(HttpRequest {
            method: Method::Post,
            url: "https://oauth2.googleapis.com/token".into(),
            headers: [(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )]
            .into(),
            body: body.into(),
        })
        .await?;
    if response.status != 200 {
        if let Ok(wire) = serde_json::from_slice::<TokenErrorWire>(&response.body)
            && !wire.error.is_empty()
            && wire.error.len() <= 64
            && wire
                .error
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            let detail = wire
                .error_description
                .as_deref()
                .map(str::to_ascii_lowercase);
            let category = detail.as_deref().and_then(|description| {
                if description.contains("client_secret") || description.contains("client secret") {
                    Some("client_secret_required")
                } else if description.contains("code_verifier")
                    || description.contains("code verifier")
                    || description.contains("code challenge")
                {
                    Some("pkce_rejected")
                } else if description.contains("redirect_uri")
                    || description.contains("redirect uri")
                {
                    Some("redirect_uri_rejected")
                } else {
                    None
                }
            });
            return Err(BackendError::new(
                BackendErrorClass::Authentication,
                match category {
                    Some(category) => {
                        format!("OAuth token exchange failed: {} ({category})", wire.error)
                    }
                    None => format!("OAuth token exchange failed: {}", wire.error),
                },
            ));
        }
        return Err(crate::error::classify_response(&response));
    }
    let wire: TokenWire = serde_json::from_slice(&response.body).map_err(|_| {
        BackendError::new(
            BackendErrorClass::Authentication,
            "OAuth token response was invalid",
        )
    })?;
    if wire.access_token.is_empty() || wire.token_type != "Bearer" || wire.expires_in == 0 {
        return Err(BackendError::new(
            BackendErrorClass::Authentication,
            "OAuth token response omitted required fields",
        ));
    }
    Ok(TokenGrant {
        access_token: Zeroizing::new(wire.access_token),
        refresh_token: wire.refresh_token.map(Zeroizing::new),
        expires_in_seconds: wire.expires_in,
        scopes: wire
            .scope
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

pub async fn refresh_access_token(
    transport: &dyn HttpTransport,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &[u8],
) -> Result<TokenGrant, BackendError> {
    let refresh_token = std::str::from_utf8(refresh_token).map_err(|_| {
        BackendError::new(
            BackendErrorClass::Authentication,
            "stored OAuth refresh token is invalid",
        )
    })?;
    if refresh_token.is_empty() {
        return Err(BackendError::new(
            BackendErrorClass::Authentication,
            "stored OAuth refresh token is empty",
        ));
    }
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("client_id", client_id);
    if let Some(client_secret) = client_secret {
        form.append_pair("client_secret", client_secret);
    }
    let body = form
        .append_pair("refresh_token", refresh_token)
        .append_pair("grant_type", "refresh_token")
        .finish()
        .into_bytes();
    let response = transport
        .execute(HttpRequest {
            method: Method::Post,
            url: "https://oauth2.googleapis.com/token".into(),
            headers: [(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )]
            .into(),
            body: body.into(),
        })
        .await?;
    if response.status != 200 {
        return Err(crate::error::classify_response(&response));
    }
    let wire: TokenWire = serde_json::from_slice(&response.body).map_err(|_| {
        BackendError::new(
            BackendErrorClass::Authentication,
            "OAuth refresh response was invalid",
        )
    })?;
    if wire.access_token.is_empty() || wire.token_type != "Bearer" || wire.expires_in == 0 {
        return Err(BackendError::new(
            BackendErrorClass::Authentication,
            "OAuth refresh response omitted required fields",
        ));
    }
    Ok(TokenGrant {
        access_token: Zeroizing::new(wire.access_token),
        refresh_token: None,
        expires_in_seconds: wire.expires_in,
        scopes: wire
            .scope
            .map(|scope| scope.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

pub async fn account_id(
    transport: &dyn HttpTransport,
    access_token: &str,
) -> Result<String, BackendError> {
    let response = transport
        .execute(HttpRequest {
            method: Method::Get,
            url: "https://www.googleapis.com/drive/v3/about?fields=user(permissionId)".into(),
            headers: [("authorization".into(), format!("Bearer {access_token}"))].into(),
            body: Default::default(),
        })
        .await?;
    if response.status != 200 {
        return Err(crate::error::classify_response(&response));
    }
    let about: DriveAbout = serde_json::from_slice(&response.body)
        .map_err(|_| BackendError::integrity("Drive account response was invalid"))?;
    if about.user.permission_id.is_empty()
        || about.user.permission_id.len() > 256
        || about.user.permission_id.chars().any(char::is_control)
    {
        return Err(BackendError::integrity(
            "Drive account identifier was invalid",
        ));
    }
    Ok(about.user.permission_id)
}

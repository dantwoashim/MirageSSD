use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{BackendError, BackendErrorClass};

use crate::http::{HttpRequest, HttpResponse, HttpTransport, Method};

const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct NativeHttpTransport {
    client: reqwest::blocking::Client,
}

impl NativeHttpTransport {
    pub fn new() -> Result<Self, BackendError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("MirageSSD/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                BackendError::new(
                    BackendErrorClass::TransientTransport,
                    "failed to initialize HTTPS transport",
                )
                .with_source(error)
            })?;
        Ok(Self { client })
    }
}

impl std::fmt::Debug for NativeHttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHttpTransport")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct RetryingHttpTransport {
    inner: Arc<dyn HttpTransport>,
    max_attempts: u32,
    requests: Arc<AtomicU64>,
    retries: Arc<AtomicU64>,
    uploaded_bytes: Arc<AtomicU64>,
    downloaded_bytes: Arc<AtomicU64>,
}

impl RetryingHttpTransport {
    pub fn new(inner: Arc<dyn HttpTransport>, max_attempts: u32) -> Result<Self, BackendError> {
        if !(1..=8).contains(&max_attempts) {
            return Err(BackendError::permanent(
                "Drive retry attempts are out of bounds",
            ));
        }
        Ok(Self {
            inner,
            max_attempts,
            requests: Arc::new(AtomicU64::new(0)),
            retries: Arc::new(AtomicU64::new(0)),
            uploaded_bytes: Arc::new(AtomicU64::new(0)),
            downloaded_bytes: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn request_count(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn retry_count(&self) -> u64 {
        self.retries.load(Ordering::Relaxed)
    }

    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded_bytes.load(Ordering::Relaxed)
    }

    pub fn downloaded_bytes(&self) -> u64 {
        self.downloaded_bytes.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl HttpTransport for RetryingHttpTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
        for attempt in 0..self.max_attempts {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.uploaded_bytes
                .fetch_add(request.body.len() as u64, Ordering::Relaxed);
            match self.inner.execute(request.clone()).await {
                Ok(response)
                    if attempt + 1 < self.max_attempts
                        && matches!(response.status, 429 | 500 | 502 | 503 | 504) =>
                {
                    self.downloaded_bytes
                        .fetch_add(response.body.len() as u64, Ordering::Relaxed);
                    self.retries.fetch_add(1, Ordering::Relaxed);
                    let delay = response
                        .header("retry-after")
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or_else(|| backoff(attempt));
                    std::thread::sleep(delay.min(Duration::from_secs(30)));
                }
                Ok(response) => {
                    self.downloaded_bytes
                        .fetch_add(response.body.len() as u64, Ordering::Relaxed);
                    return Ok(response);
                }
                Err(error)
                    if attempt + 1 < self.max_attempts
                        && matches!(
                            error.class,
                            BackendErrorClass::RateLimit | BackendErrorClass::TransientTransport
                        ) =>
                {
                    self.retries.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(
                        error
                            .retry_after
                            .unwrap_or_else(|| backoff(attempt))
                            .min(Duration::from_secs(30)),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Err(BackendError::permanent(
            "Drive retry loop exhausted unexpectedly",
        ))
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(100_u64.saturating_mul(1_u64 << attempt.min(6)))
}

#[async_trait]
impl HttpTransport for NativeHttpTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, BackendError> {
        let parsed = reqwest::Url::parse(&request.url)
            .map_err(|_| BackendError::permanent("backend request URL is invalid"))?;
        if parsed.scheme() != "https" {
            return Err(BackendError::permanent("backend transport requires HTTPS"));
        }

        let mut builder = match request.method {
            Method::Get => self.client.get(parsed),
            Method::Post => self.client.post(parsed),
            Method::Put => self.client.put(parsed),
            Method::Delete => self.client.delete(parsed),
        };
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if !request.body.is_empty() {
            builder = builder.body(request.body);
        }

        let response = builder.send().map_err(|error| {
            BackendError::new(
                BackendErrorClass::TransientTransport,
                if error.is_timeout() {
                    "backend request timed out"
                } else {
                    "backend HTTPS request failed"
                },
            )
            .with_source(error)
        })?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES)
        {
            return Err(BackendError::integrity(
                "backend response exceeded the bounded body limit",
            ));
        }

        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            let value = value
                .to_str()
                .map_err(|_| BackendError::integrity("backend returned a malformed header"))?;
            headers.insert(name.as_str().to_owned(), value.to_owned());
        }
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|error| {
                BackendError::new(
                    BackendErrorClass::TransientTransport,
                    "backend response body could not be read",
                )
                .with_source(error)
            })?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(BackendError::integrity(
                "backend response exceeded the bounded body limit",
            ));
        }
        Ok(HttpResponse {
            status,
            headers,
            body: Bytes::from(body),
        })
    }
}

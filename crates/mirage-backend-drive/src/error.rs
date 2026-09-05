use crate::http::HttpResponse;
use mirage_backend::{BackendError, BackendErrorClass};
use std::time::Duration;

pub fn classify_response(response: &HttpResponse) -> BackendError {
    let body = String::from_utf8_lossy(&response.body).to_ascii_lowercase();
    let class = match response.status {
        401 => BackendErrorClass::Authentication,
        403 if body.contains("ratelimit")
            || body.contains("quota")
            || body.contains("userratelimitexceeded") =>
        {
            BackendErrorClass::RateLimit
        }
        403 => BackendErrorClass::Permission,
        404 => BackendErrorClass::Missing,
        408 | 429 | 500..=599 => {
            if response.status == 429 {
                BackendErrorClass::RateLimit
            } else {
                BackendErrorClass::TransientTransport
            }
        }
        _ => BackendErrorClass::Permanent,
    };
    let mut error = BackendError::new(
        class,
        format!("Drive request failed with HTTP {}", response.status),
    );
    if matches!(
        class,
        BackendErrorClass::RateLimit | BackendErrorClass::TransientTransport
    ) && let Some(seconds) = response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value <= 86_400)
    {
        error = error.with_retry_after(Duration::from_secs(seconds));
    }
    error
}

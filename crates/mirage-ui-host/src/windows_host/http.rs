//! Loopback HTTP request parsing, responses, and authorization.

use super::*;

const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;

const MAX_HTTP_BODY_BYTES: usize = MAX_FRAME_BYTES;

/// The per-launch bridge token is the secret that authorizes a request;
/// only the page this host opened knows it. Same-origin evidence is a
/// second check, but browsers send `Origin` only on POST — a same-origin
/// GET carries none — so its absence must not refuse the page: when it
/// is present it must match, otherwise `Sec-Fetch-Site`/`Referer` must
/// say same-origin. Requiring `Origin` on GET left every fresh install's
/// sign-in status check at 403 in browsers that follow the spec.
pub(super) fn authorize(request: &HttpRequest, origin: &str, token: &str) -> Result<(), Vec<u8>> {
    let header = |name: &str| request.headers.get(name).map(String::as_str);
    let token_ok = header("x-mirage-token").is_some_and(|value| constant_time_eq(value, token));
    let origin_ok = match header("origin") {
        Some(supplied) => supplied == origin,
        None => {
            header("sec-fetch-site") == Some("same-origin")
                || header("referer").is_some_and(|referer| {
                    referer == origin || referer.starts_with(&format!("{origin}/"))
                })
        }
    };
    if !token_ok || !origin_ok {
        return Err(br#"{"error":"forbidden"}"#.to_vec());
    }
    Ok(())
}

pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
}

pub(super) fn read_http_request(
    stream: &mut TcpStream,
) -> Result<HttpRequest, Box<dyn std::error::Error>> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if bytes.len() >= MAX_HTTP_HEADER_BYTES {
            return Err("HTTP request headers exceed the bridge limit".into());
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err("HTTP request ended before its headers".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let (method, path, headers, content_length) = {
        let header_text = std::str::from_utf8(&bytes[..header_end])?;
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().ok_or("missing HTTP request line")?;
        let mut request_parts = request_line.split_ascii_whitespace();
        let method = request_parts
            .next()
            .ok_or("missing HTTP method")?
            .to_owned();
        let path = request_parts
            .next()
            .ok_or("missing HTTP path")?
            .split('?')
            .next()
            .ok_or("missing HTTP path")?
            .to_owned();
        let version = request_parts.next().ok_or("missing HTTP version")?;
        if request_parts.next().is_some() || version != "HTTP/1.1" {
            return Err("unsupported HTTP request line".into());
        }
        let mut headers = BTreeMap::new();
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').ok_or("malformed HTTP header")?;
            let name = name.trim().to_ascii_lowercase();
            if headers.insert(name, value.trim().to_owned()).is_some() {
                return Err("duplicate HTTP header".into());
            }
        }
        let content_length = headers
            .get("content-length")
            .map(|value| value.parse::<usize>())
            .transpose()?
            .unwrap_or(0);
        (method, path, headers, content_length)
    };
    if content_length > MAX_HTTP_BODY_BYTES {
        return Err("HTTP request body exceeds the bridge limit".into());
    }
    let total = header_end
        .checked_add(content_length)
        .ok_or("HTTP request length overflow")?;
    while bytes.len() < total {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err("HTTP request body ended early".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > total {
            return Err("HTTP pipelining is not supported".into());
        }
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..total].to_vec(),
    })
}

pub(super) fn write_file(
    stream: &mut TcpStream,
    path: &Path,
    content_type: &str,
    immutable: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = std::fs::read(path)?;
    write_response(stream, 200, content_type, &body, immutable)?;
    Ok(())
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    immutable: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    };
    let cache = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: {cache}\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'\r\nCross-Origin-Resource-Policy: same-origin\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

pub(super) fn random_token() -> Result<String, MirageError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("UI bridge token generation failed"))?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}")
            .map_err(|_| MirageError::internal_invariant("UI token encoding failed"))?;
    }
    Ok(token)
}

pub(super) fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

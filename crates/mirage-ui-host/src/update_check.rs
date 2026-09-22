//! `GET /api/update/check` — latest GitHub release vs the running version,
//! cached for six hours; network failures are silent (offline = no banner).

use std::sync::Mutex;
use std::time::{Duration, Instant};

const RELEASES_URL: &str = "https://api.github.com/repos/dantwoashim/MirageSSD/releases/latest";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

struct Cache {
    checked_at: Option<Instant>,
    payload: serde_json::Value,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn parse_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let digits: Vec<u64> = tag
        .trim_start_matches('v')
        .split('-')
        .next()?
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<_, _>>()
        .ok()?;
    if digits.len() >= 3 {
        Some((digits[0], digits[1], digits[2]))
    } else {
        None
    }
}

fn current() -> (u64, u64, u64) {
    parse_tag(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

pub fn check() -> serde_json::Value {
    {
        let guard = CACHE.lock().unwrap();
        if let Some(cache) = guard.as_ref()
            && cache.checked_at.is_some_and(|at| at.elapsed() < CACHE_TTL)
        {
            return cache.payload.clone();
        }
    }
    let fetched = fetch();
    let mut guard = CACHE.lock().unwrap();
    *guard = Some(Cache {
        checked_at: Some(Instant::now()),
        payload: fetched.clone(),
    });
    fetched
}

fn fetch() -> serde_json::Value {
    let Ok(response) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("MirageSSD-update-check")
        .build()
        .and_then(|client| client.get(RELEASES_URL).send())
    else {
        return serde_json::json!({"ok": false, "checked": false});
    };
    let Some(body) = response
        .text()
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return serde_json::json!({"ok": false, "checked": false});
    };
    let tag = body["tag_name"].as_str().unwrap_or_default();
    let url = body["html_url"].as_str().unwrap_or_default();
    let newer = parse_tag(tag).is_some_and(|tag| tag > current());
    serde_json::json!({
        "ok": true,
        "checked": true,
        "checked_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|s| s.as_secs())
            .unwrap_or(0),
        "current": env!("CARGO_PKG_VERSION"),
        "channel": "preview",
        "latest": tag,
        "url": url,
        "update_available": newer,
    })
}

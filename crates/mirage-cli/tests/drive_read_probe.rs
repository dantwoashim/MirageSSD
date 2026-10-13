//! Live Google Drive range-read probe (ignored): measures time to first byte,
//! per-request throughput, offset sensitivity, and concurrency scaling of
//! `files.get?alt=media` range requests over HTTP/1.1 and HTTP/2, reading
//! existing payload objects of the signed-in user's volumes. Read-only.
//! `cargo test --release -p mirage-cli --test drive_read_probe -- --ignored --nocapture`

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

struct Object {
    id: String,
    size: u64,
}

struct Sample {
    ttfb_ms: f64,
    total_ms: f64,
    bytes: u64,
}

fn token() -> Zeroizing<String> {
    let credentials = mirage_cli::commands::backend_login::oauth_client_credentials(Some(
        std::path::PathBuf::from(r"C:\Program Files\MirageSSD\oauth-desktop.json"),
    ))
    .expect("installed oauth-desktop.json");
    mirage_cli::commands::drive_live::connect(&credentials, None)
        .expect("session")
        .access_token
}

fn h1_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .http1_only()
        .timeout(Duration::from_secs(300))
        .build()
        .expect("h1 client")
}

fn h2_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .http2_prior_knowledge()
        .http2_adaptive_window(true)
        .timeout(Duration::from_secs(300))
        .build()
        .expect("h2 client")
}

fn list_objects(client: &reqwest::blocking::Client, token: &str) -> Vec<Object> {
    #[derive(serde::Deserialize)]
    struct List {
        files: Vec<File>,
        #[serde(rename = "nextPageToken")]
        next: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct File {
        id: String,
        size: Option<String>,
    }
    let mut objects = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..10 {
        let mut url = url::Url::parse("https://www.googleapis.com/drive/v3/files").unwrap();
        url.query_pairs_mut()
            .append_pair("q", "trashed = false and name contains 'payload-'")
            .append_pair("spaces", "drive")
            .append_pair("pageSize", "1000")
            .append_pair("fields", "nextPageToken,files(id,size)");
        if let Some(page) = &page {
            url.query_pairs_mut().append_pair("pageToken", page);
        }
        let body = client
            .get(url)
            .bearer_auth(token)
            .send()
            .expect("list")
            .bytes()
            .expect("list body");
        let list: List = serde_json::from_slice(&body).expect("list json");
        objects.extend(list.files.into_iter().filter_map(|file| {
            let size = file.size?.parse().ok()?;
            (size >= 16 << 20).then_some(Object { id: file.id, size })
        }));
        page = list.next;
        if page.is_none() || objects.len() >= 2000 {
            break;
        }
    }
    objects
}

fn range_get(
    client: &reqwest::blocking::Client,
    token: &str,
    object: &Object,
    offset: u64,
    length: u64,
) -> Sample {
    let url = format!(
        "https://www.googleapis.com/drive/v3/files/{}?alt=media",
        object.id
    );
    let started = Instant::now();
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("range", format!("bytes={}-{}", offset, offset + length - 1))
        .send()
        .expect("range get");
    let ttfb = started.elapsed();
    assert_eq!(response.status().as_u16(), 206, "range status");
    let mut body = Vec::with_capacity(length as usize);
    response
        .take(length + 1)
        .read_to_end(&mut body)
        .expect("body");
    assert_eq!(body.len() as u64, length, "range length");
    Sample {
        ttfb_ms: ttfb.as_secs_f64() * 1e3,
        total_ms: started.elapsed().as_secs_f64() * 1e3,
        bytes: length,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, objects: &'a [Object]) -> &'a Object {
        &objects[(self.next() % objects.len() as u64) as usize]
    }
    fn offset(&mut self, object: &Object, length: u64) -> u64 {
        (self.next() % (object.size - length)) & !4095
    }
}

fn pct(values: &mut [f64], q: f64) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[((values.len() - 1) as f64 * q).round() as usize]
}

fn report(label: &str, samples: &[Sample]) {
    let mut ttfb: Vec<f64> = samples.iter().map(|s| s.ttfb_ms).collect();
    let mut total: Vec<f64> = samples.iter().map(|s| s.total_ms).collect();
    let mut body_rate: Vec<f64> = samples
        .iter()
        .map(|s| s.bytes as f64 / (1 << 20) as f64 / ((s.total_ms - s.ttfb_ms).max(0.5) / 1e3))
        .collect();
    eprintln!(
        "PROBE {label:<28} n={:<3} ttfb p50={:7.1} p90={:7.1} max={:7.1} | total p50={:7.1} p90={:7.1} | body MiB/s p50={:6.2}",
        samples.len(),
        pct(&mut ttfb, 0.5),
        pct(&mut ttfb, 0.9),
        pct(&mut ttfb, 1.0),
        pct(&mut total, 0.5),
        pct(&mut total, 0.9),
        pct(&mut body_rate, 0.5),
    );
}

fn sizes_and_offsets(
    name: &str,
    client: &reqwest::blocking::Client,
    token: &str,
    objects: &[Object],
    rng: &mut Rng,
) {
    // Warm the connection first so handshakes do not count.
    let object = rng.pick(objects);
    let _ = range_get(client, token, object, 0, 4096);
    for (label, length) in [
        ("16K", 16u64 << 10),
        ("64K", 64 << 10),
        ("256K", 256 << 10),
        ("1M", 1 << 20),
        ("4M", 4 << 20),
    ] {
        let samples: Vec<Sample> = (0..10)
            .map(|_| {
                let object = rng.pick(objects);
                let offset = rng.offset(object, length);
                range_get(client, token, object, offset, length)
            })
            .collect();
        report(&format!("{name} warm {label}"), &samples);
    }
    let object = rng.pick(objects);
    for fraction in [0u64, 1, 2, 3] {
        let offset = ((object.size - (64 << 10)) * fraction / 3) & !4095;
        let samples: Vec<Sample> = (0..5)
            .map(|_| range_get(client, token, object, offset, 64 << 10))
            .collect();
        report(&format!("{name} 64K at {}MiB", offset >> 20), &samples);
    }
    let object = rng.pick(objects);
    let length = (16u64 << 20).min(object.size);
    let sample = range_get(client, token, object, 0, length);
    report(&format!("{name} one 16M request"), &[sample]);
}

fn concurrency(
    name: &str,
    client: &reqwest::blocking::Client,
    token: &str,
    objects: &Arc<Vec<Object>>,
    seed: u64,
) {
    for threads in [1usize, 4, 16, 32] {
        // Round 0 warms the pool (handshakes), round 1 is measured.
        for round in 0..2 {
            let started = Instant::now();
            let handles: Vec<_> = (0..threads)
                .map(|thread| {
                    let client = client.clone();
                    let token = token.to_owned();
                    let objects = Arc::clone(objects);
                    std::thread::spawn(move || {
                        let mut rng =
                            Rng(seed ^ ((thread as u64 + 1) * 0x9e37_79b9_7f4a_7c15) ^ round);
                        (0..3)
                            .map(|_| {
                                let object = rng.pick(&objects);
                                let offset = rng.offset(object, 1 << 20);
                                range_get(&client, &token, object, offset, 1 << 20)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let samples: Vec<Sample> = handles
                .into_iter()
                .flat_map(|handle| handle.join().expect("worker"))
                .collect();
            let elapsed = started.elapsed().as_secs_f64();
            if round == 1 {
                let bytes: u64 = samples.iter().map(|s| s.bytes).sum();
                report(&format!("{name} x{threads} 1M"), &samples);
                eprintln!(
                    "PROBE {name} x{threads} aggregate {:.2} MiB/s over {:.1}s",
                    bytes as f64 / (1 << 20) as f64 / elapsed,
                    elapsed
                );
            }
        }
    }
}

#[test]
#[ignore = "talks to the live Google Drive of the signed-in user"]
fn drive_range_read_characteristics() {
    let token = token();
    let h1 = h1_client();
    let objects = Arc::new(list_objects(&h1, &token));
    assert!(!objects.is_empty(), "no payload objects >= 16 MiB visible");
    eprintln!("PROBE {} payload objects >= 16 MiB", objects.len());
    let mut rng = Rng(0x5eed_2026_1012);

    // Fresh connection per request: DNS + TCP + TLS + request.
    let fresh: Vec<Sample> = (0..8)
        .map(|_| {
            let object = rng.pick(&objects);
            let offset = rng.offset(object, 64 << 10);
            range_get(&h1_client(), &token, object, offset, 64 << 10)
        })
        .collect();
    report("h1 fresh-conn 64K", &fresh);

    sizes_and_offsets("h1", &h1, &token, &objects, &mut rng);
    concurrency("h1", &h1, &token, &objects, 11);

    let h2 = h2_client();
    sizes_and_offsets("h2", &h2, &token, &objects, &mut rng);
    concurrency("h2", &h2, &token, &objects, 23);
}

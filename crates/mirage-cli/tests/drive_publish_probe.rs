//! Live Google Drive publication probe (ignored): times upload and readback
//! of one encrypted-size object under the signed-in user's token, so the
//! publisher's bottleneck can be measured without the service or elevation.
//! `cargo test --release -p mirage-cli --test drive_publish_probe -- --ignored --nocapture`

use std::sync::Arc;
use std::time::Instant;

use mirage_backend::{ObjectBackend, ObjectKind, UploadSource};
use mirage_types::{ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

#[test]
#[ignore = "talks to the live Google Drive of the signed-in user"]
fn time_one_payload_upload_and_readback() {
    let credentials = mirage_cli::commands::backend_login::oauth_client_credentials(Some(
        std::path::PathBuf::from(r"C:\Program Files\MirageSSD\oauth-desktop.json"),
    ))
    .expect("installed oauth-desktop.json");
    let session = mirage_cli::commands::drive_live::connect(&credentials, None).expect("session");
    let backend = mirage_backend_drive::DriveObjectBackend::new(
        session.transport.clone() as Arc<dyn mirage_backend_drive::HttpTransport>,
        session.access_token.clone(),
        RepositoryId::from_bytes(*b"mirage-probe-000"),
    )
    .expect("backend");
    let size: usize = std::env::var("PROBE_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
        << 20;
    let mut bytes = vec![0u8; size];
    getrandom::fill(&mut bytes).expect("random");
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let started = Instant::now();
    let reference = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Payload,
        UploadSource::from_bytes(bytes::Bytes::from(bytes)),
        hash,
        CancellationToken::new(),
    ))
    .unwrap_or_else(|error| panic!("upload failed: {error:?}"));
    let upload = started.elapsed().as_secs_f64();
    let started = Instant::now();
    futures_executor::block_on(mirage_backend::verify_object_bytes(
        &backend,
        &reference,
        CancellationToken::new(),
    ))
    .unwrap_or_else(|error| panic!("readback failed: {error:?}"));
    let readback = started.elapsed().as_secs_f64();
    eprintln!(
        "PROBE transport: requests={} retries={}",
        session.transport.request_count(),
        session.transport.retry_count()
    );
    let mib = size as f64 / (1 << 20) as f64;
    eprintln!(
        "PROBE {mib:.0} MiB: upload {upload:.1}s ({:.2} MB/s), readback {readback:.1}s ({:.2} MB/s)",
        mib / upload,
        mib / readback
    );
}

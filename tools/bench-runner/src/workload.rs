//! Read-only trace replay against a mounted (or plain) directory tree.
//!
//! Input is the `miragessd-read-trace-v1` JSONL format produced by the
//! benchmark fixture generator: a header record followed by `open`, `read`
//! and `close` operations on small integer handles. Every operation is timed
//! with a monotonic clock and emitted as one JSONL result record, so failed
//! and slow calls are preserved rather than averaged away.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

/// Explicit cache/restart state label for a run, per the benchmark protocol.
/// The label is recorded on every result row; establishing the state itself
/// (for example a reboot for a cold OS cache) is the harness's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    FirstBootstrap,
    WarmNamespace,
    WarmDiskColdOs,
    WarmOs,
    CleanRestart,
    DirtyRestart,
    OfflineStart,
    MidOperationOutage,
}

impl CacheState {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "first-bootstrap" => Self::FirstBootstrap,
            "warm-namespace" => Self::WarmNamespace,
            "warm-disk-cold-os" => Self::WarmDiskColdOs,
            "warm-os" => Self::WarmOs,
            "clean-restart" => Self::CleanRestart,
            "dirty-restart" => Self::DirtyRestart,
            "offline-start" => Self::OfflineStart,
            "mid-operation-outage" => Self::MidOperationOutage,
            _ => return None,
        })
    }
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::FirstBootstrap => "first-bootstrap",
            Self::WarmNamespace => "warm-namespace",
            Self::WarmDiskColdOs => "warm-disk-cold-os",
            Self::WarmOs => "warm-os",
            Self::CleanRestart => "clean-restart",
            Self::DirtyRestart => "dirty-restart",
            Self::OfflineStart => "offline-start",
            Self::MidOperationOutage => "mid-operation-outage",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub root: PathBuf,
    pub state: CacheState,
    pub arm: String,
    /// Best-effort bypass of the OS page cache via FILE_FLAG_NO_BUFFERING on
    /// Windows. Reads must be sector-aligned; unaligned requests are reported
    /// as failures rather than silently re-buffered.
    pub no_buffering: bool,
    /// Per-handle SHA-256 over the concatenated bytes returned by reads, for
    /// external comparison against the fixture oracle.
    pub hash_reads: bool,
}

#[derive(Debug, Default)]
pub struct RunSummary {
    pub ops: u64,
    pub errors: u64,
    pub bytes_read: u64,
    pub read_latencies_ns: Vec<u64>,
    pub wall_ns: u64,
}

impl RunSummary {
    #[must_use]
    pub fn percentile_ns(sorted: &[u64], percentile: f64) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let index = ((sorted.len() as f64 - 1.0) * percentile).ceil() as usize;
        sorted[index.min(sorted.len() - 1)]
    }
}

struct OpenHandle {
    file: File,
    hasher: Option<sha2::Sha256>,
}

/// Replay the trace, writing one JSON record per operation plus a header and
/// summary. I/O errors are recorded on the operation row and counted; the run
/// continues so a failing mount cannot silently truncate the evidence.
pub fn replay(
    trace: &str,
    config: &RunConfig,
    out: &mut impl Write,
) -> Result<RunSummary, std::io::Error> {
    let mut handles: BTreeMap<u64, OpenHandle> = BTreeMap::new();
    let mut summary = RunSummary::default();
    let started = Instant::now();
    let mut sequence = 0u64;
    let mut header_written = false;

    for raw_line in trace.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let record: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                write_row(
                    out,
                    &json!({
                        "type": "error", "seq": sequence,
                        "error": format!("unparseable trace line: {error}"),
                    }),
                )?;
                summary.errors += 1;
                continue;
            }
        };
        if record.get("type").and_then(Value::as_str) == Some("header") {
            write_row(
                out,
                &json!({
                    "type": "run_header",
                    "runner": "mirage-bench-runner/workload",
                    "trace_header": record,
                    "arm": config.arm,
                    "state": config.state.label(),
                    "no_buffering": config.no_buffering,
                    "root": config.root,
                }),
            )?;
            header_written = true;
            continue;
        }
        sequence += 1;
        let op = record.get("op").and_then(Value::as_str).unwrap_or("");
        let handle = record.get("handle").and_then(Value::as_u64);
        let op_start = Instant::now();
        let mut row = json!({
            "type": "op", "seq": sequence, "op": op, "handle": handle,
        });
        match op {
            "open" => {
                let path = record.get("path").and_then(Value::as_str).unwrap_or("");
                match (
                    handle,
                    open_relative(&config.root, path, config.no_buffering),
                ) {
                    (Some(handle), Ok(file)) => {
                        let hasher = config.hash_reads.then(|| {
                            use sha2::Digest;
                            sha2::Sha256::new()
                        });
                        handles.insert(handle, OpenHandle { file, hasher });
                        row["ok"] = json!(true);
                        row["path"] = json!(path);
                    }
                    (Some(_), Err(error)) => {
                        row["ok"] = json!(false);
                        row["path"] = json!(path);
                        row["error"] = json!(error.to_string());
                        summary.errors += 1;
                    }
                    _ => {
                        row["ok"] = json!(false);
                        row["error"] = json!("open operation is missing a handle");
                        summary.errors += 1;
                    }
                }
            }
            "read" => {
                let offset = record.get("offset").and_then(Value::as_u64).unwrap_or(0);
                let length = record.get("length").and_then(Value::as_u64).unwrap_or(0);
                row["offset"] = json!(offset);
                row["length"] = json!(length);
                let Some(open) = handle.and_then(|id| handles.get_mut(&id)) else {
                    row["ok"] = json!(false);
                    row["error"] = json!("read on an unopened handle");
                    summary.errors += 1;
                    write_timed(out, &row, op_start)?;
                    summary.ops += 1;
                    continue;
                };
                let mut buffer = vec![0u8; usize::try_from(length).unwrap_or(0)];
                match open
                    .file
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| open.file.read(&mut buffer))
                {
                    Ok(read) => {
                        if let Some(hasher) = open.hasher.as_mut() {
                            use sha2::Digest;
                            hasher.update(&buffer[..read]);
                        }
                        row["ok"] = json!(true);
                        row["bytes"] = json!(read);
                        summary.bytes_read += read as u64;
                    }
                    Err(error) => {
                        row["ok"] = json!(false);
                        row["error"] = json!(error.to_string());
                        summary.errors += 1;
                    }
                }
            }
            "close" => match handle.and_then(|id| handles.remove(&id)) {
                Some(mut open) => {
                    if let Some(hasher) = open.hasher.take() {
                        use sha2::Digest;
                        row["read_sha256"] = json!(format!("{:x}", hasher.finalize()));
                    }
                    row["ok"] = json!(true);
                }
                None => {
                    row["ok"] = json!(false);
                    row["error"] = json!("close on an unopened handle");
                    summary.errors += 1;
                }
            },
            other => {
                row["ok"] = json!(false);
                row["error"] = json!(format!("unsupported operation: {other}"));
                summary.errors += 1;
            }
        }
        write_timed(out, &row, op_start)?;
        summary.ops += 1;
        if op == "read" {
            summary
                .read_latencies_ns
                .push(op_start.elapsed().as_nanos() as u64);
        }
    }
    summary.wall_ns = started.elapsed().as_nanos() as u64;
    if !header_written {
        write_row(
            out,
            &json!({
                "type": "run_header",
                "runner": "mirage-bench-runner/workload",
                "trace_header": Value::Null,
                "arm": config.arm,
                "state": config.state.label(),
                "no_buffering": config.no_buffering,
                "root": config.root,
            }),
        )?;
    }
    let mut latencies = summary.read_latencies_ns.clone();
    latencies.sort_unstable();
    write_row(
        out,
        &json!({
            "type": "run_summary",
            "ops": summary.ops,
            "errors": summary.errors,
            "bytes_read": summary.bytes_read,
            "reads": latencies.len(),
            "read_p50_ns": RunSummary::percentile_ns(&latencies, 0.50),
            "read_p95_ns": RunSummary::percentile_ns(&latencies, 0.95),
            "read_p99_ns": RunSummary::percentile_ns(&latencies, 0.99),
            "wall_ns": summary.wall_ns,
            "handles_leaked": handles.len(),
        }),
    )?;
    Ok(summary)
}

/// Open a path below the run root. Refuses absolute paths and parent
/// traversal so a corrupted trace cannot escape the mounted tree.
fn open_relative(root: &Path, relative: &str, no_buffering: bool) -> Result<File, std::io::Error> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "trace path escapes the run root",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    if no_buffering {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_RANDOM_ACCESS: u32 = 0x1000_0000;
        options.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_RANDOM_ACCESS);
    }
    let _ = no_buffering;
    options.open(root.join(path))
}

fn write_row(out: &mut impl Write, row: &Value) -> Result<(), std::io::Error> {
    let mut line = serde_json::to_vec(row)?;
    line.push(b'\n');
    out.write_all(&line)
}

fn write_timed(out: &mut impl Write, row: &Value, op_start: Instant) -> Result<(), std::io::Error> {
    let mut row = row.clone();
    row["latency_ns"] = json!(op_start.elapsed().as_nanos() as u64);
    write_row(out, &row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &Path) -> RunConfig {
        RunConfig {
            root: root.to_path_buf(),
            state: CacheState::WarmOs,
            arm: "test-arm".to_owned(),
            no_buffering: false,
            hash_reads: true,
        }
    }

    #[test]
    fn replays_open_read_close_and_hashes_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.bin"), b"abcdefgh").unwrap();
        let trace = concat!(
            "{\"type\":\"header\",\"format\":\"miragessd-read-trace-v1\"}\n",
            "{\"op\":\"open\",\"handle\":1,\"path\":\"file.bin\"}\n",
            "{\"op\":\"read\",\"handle\":1,\"offset\":2,\"length\":4}\n",
            "{\"op\":\"close\",\"handle\":1}\n"
        );
        let mut out = Vec::new();
        let summary = replay(trace, &config(dir.path()), &mut out).unwrap();
        assert_eq!(summary.ops, 3);
        assert_eq!(summary.errors, 0);
        assert_eq!(summary.bytes_read, 4);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"type\":\"run_header\""));
        assert!(text.contains("\"bytes\":4"));
        use sha2::Digest;
        let expected = format!("{:x}", sha2::Sha256::digest(b"cdef"));
        assert!(text.contains(&expected));
    }

    #[test]
    fn records_failures_without_stopping() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.bin"), b"xy").unwrap();
        let trace = concat!(
            "{\"op\":\"open\",\"handle\":1,\"path\":\"missing.bin\"}\n",
            "{\"op\":\"read\",\"handle\":1,\"offset\":0,\"length\":4}\n",
            "{\"op\":\"read\",\"handle\":9,\"offset\":0,\"length\":4}\n",
            "{\"op\":\"open\",\"handle\":2,\"path\":\"ok.bin\"}\n",
            "{\"op\":\"read\",\"handle\":2,\"offset\":0,\"length\":2}\n",
            "{\"op\":\"close\",\"handle\":2}\n"
        );
        let mut out = Vec::new();
        let summary = replay(trace, &config(dir.path()), &mut out).unwrap();
        assert_eq!(summary.ops, 6);
        assert_eq!(summary.errors, 3);
        assert_eq!(summary.bytes_read, 2);
    }

    #[test]
    fn rejects_paths_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let trace = concat!(
            "{\"op\":\"open\",\"handle\":1,\"path\":\"../escape.txt\"}\n",
            "{\"op\":\"open\",\"handle\":2,\"path\":\"D:/absolute.txt\"}\n"
        );
        let mut out = Vec::new();
        let summary = replay(trace, &config(dir.path()), &mut out).unwrap();
        assert_eq!(summary.errors, 2);
    }

    #[test]
    fn states_round_trip_labels() {
        for label in [
            "first-bootstrap",
            "warm-namespace",
            "warm-disk-cold-os",
            "warm-os",
            "clean-restart",
            "dirty-restart",
            "offline-start",
            "mid-operation-outage",
        ] {
            assert_eq!(CacheState::parse(label).unwrap().label(), label);
        }
        assert!(CacheState::parse("warm").is_none());
    }
}

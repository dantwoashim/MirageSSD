//! Read-only trace replay runner for the MirageSSD benchmark protocol.
//!
//! Usage:
//!   workload --root M:\ --trace trace.jsonl --out results.jsonl
//!            --arm rclone-full --state warm-disk-cold-os [--no-buffering]
//!            [--hash-reads]
//!
//! Writes one JSONL result record per replayed operation plus a run header and
//! summary. Establishing the requested cache state (reboot, remount, network
//! shaping) is the harness's responsibility; this runner never mutates the
//! mounted tree.

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use mirage_bench_runner::workload::{self, CacheState, RunConfig};

fn main() -> ExitCode {
    let mut root: Option<PathBuf> = None;
    let mut trace: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut arm = "unlabeled".to_owned();
    let mut state: Option<CacheState> = None;
    let mut no_buffering = false;
    let mut hash_reads = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = args.next().map(PathBuf::from),
            "--trace" => trace = args.next().map(PathBuf::from),
            "--out" => out = args.next().map(PathBuf::from),
            "--arm" => arm = args.next().unwrap_or_else(|| "unlabeled".to_owned()),
            "--state" => {
                let value = args.next().unwrap_or_default();
                state = CacheState::parse(&value);
                if state.is_none() {
                    eprintln!("unknown --state {value}; expected an explicit cache/restart state");
                    return ExitCode::from(2);
                }
            }
            "--no-buffering" => no_buffering = true,
            "--hash-reads" => hash_reads = true,
            "--help" | "-h" => {
                println!(
                    "workload --root DIR --trace FILE --out FILE --state STATE [--arm LABEL] [--no-buffering] [--hash-reads]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(root), Some(trace), Some(out), Some(state)) = (root, trace, out, state) else {
        eprintln!("--root, --trace, --out and --state are required");
        return ExitCode::from(2);
    };
    if !root.is_dir() {
        eprintln!("--root {} is not a directory", root.display());
        return ExitCode::from(2);
    }
    let trace_text = match std::fs::read_to_string(&trace) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cannot read {}: {error}", trace.display());
            return ExitCode::from(1);
        }
    };
    let parent = out.parent().map(PathBuf::from).unwrap_or_default();
    if !parent.as_os_str().is_empty()
        && let Err(error) = std::fs::create_dir_all(&parent)
    {
        eprintln!("cannot create {}: {error}", parent.display());
        return ExitCode::from(1);
    }
    let file = match std::fs::File::create(&out) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("cannot create {}: {error}", out.display());
            return ExitCode::from(1);
        }
    };
    let config = RunConfig {
        root,
        state,
        arm,
        no_buffering,
        hash_reads,
    };
    let mut writer = BufWriter::new(file);
    let result = workload::replay(&trace_text, &config, &mut writer)
        .and_then(|summary| writer.flush().map(|()| summary));
    match result {
        Ok(summary) => {
            eprintln!(
                "done: {} ops, {} errors, {} bytes in {} ms",
                summary.ops,
                summary.errors,
                summary.bytes_read,
                summary.wall_ns / 1_000_000
            );
            if summary.errors == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
        Err(error) => {
            eprintln!("replay failed: {error}");
            ExitCode::from(1)
        }
    }
}

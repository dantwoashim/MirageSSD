"""Compile the real range scheduler in isolation against a latency-only mock.

The production function is extracted verbatim. Only its called backend is
replaced; no Mirage volume, credentials, network or workspace build is used.
This proves scheduling/byte assembly, not live Drive or mounted performance.
Supply a real rustc.exe path to avoid rustup downloads.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


PREAMBLE = r'''
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
const SUBRANGE_BYTES: u64 = 1024 * 1024;
const SUBRANGE_PARALLELISM: usize = 4;
#[derive(Clone, Copy, Debug)] enum FetchFailure { Backend, Integrity }
mod mirage_types {
    #[derive(Clone, Copy)] pub struct CheckedRange(u64, u64);
    impl CheckedRange {
        pub fn try_new(start: u64, len: u64) -> Option<Self> {
            if len == 0 || start.checked_add(len).is_none() {None} else {Some(Self(start, len))}
        }
        pub fn start(self) -> u64 {self.0}
        pub fn len(self) -> u64 {self.1}
    }
}
mod mirage_backend {
    pub trait ObjectBackend: Send + Sync {}
    pub struct Mock;
    impl ObjectBackend for Mock {}
    pub struct RemoteObjectRef;
}
static EVENTS: Mutex<Vec<(u64,u64,u128,u128)>> = Mutex::new(Vec::new());
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static DELAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn fetch_remote_part(_: &Arc<dyn mirage_backend::ObjectBackend>,
    _: &mirage_backend::RemoteObjectRef, r: mirage_types::CheckedRange)
    -> Result<Vec<u8>, FetchFailure> {
    let epoch = EPOCH.get_or_init(Instant::now);
    let start = epoch.elapsed().as_micros();
    std::thread::sleep(Duration::from_millis(DELAY.load(std::sync::atomic::Ordering::Relaxed)));
    let bytes = (r.start()..r.start()+r.len()).map(|i| (i % 251) as u8).collect();
    let finish = epoch.elapsed().as_micros();
    EVENTS.lock().unwrap().push((r.start(),r.len(),start,finish));
    Ok(bytes)
}
'''

MAIN = r'''
fn main() {
    let backend: Arc<dyn mirage_backend::ObjectBackend> = Arc::new(mirage_backend::Mock);
    let object = mirage_backend::RemoteObjectRef;
    // Boundary and randomized-range assembly tests against exact bytes.
    let mut lengths = vec![1, 68, 1048575, 1048576, 1048577, 4194304, 4194372];
    let mut rng = 20261012u64;
    for _ in 0..64 {rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; lengths.push(1 + rng % 4194372);}
    for len in &lengths {
        let range = mirage_types::CheckedRange::try_new(1234,*len).unwrap();
        for fun in [fetch_remote_range, fetch_balanced_range] {
            EVENTS.lock().unwrap().clear();
            let bytes = fun(&backend,&object,range).unwrap();
            assert_eq!(bytes.len(),*len as usize);
            assert!(bytes.iter().enumerate().all(|(i,b)| *b == ((1234+i as u64)%251) as u8));
            let mut events = EVENTS.lock().unwrap().clone(); events.sort_by_key(|e|e.0);
            let mut cursor = 1234;
            for (offset,length,_,_) in events {assert_eq!(offset,cursor);cursor+=length;}
            assert_eq!(cursor,1234+len);
        }
    }
    DELAY.store(60,std::sync::atomic::Ordering::Relaxed);
    println!("{{\"kind\":\"actual_source_mock_backend\",\"mock_delay_ms\":60,\"assembly_cases_per_variant\":{},\"samples\":[",lengths.len());
    let mut first = true;
    for run in 0..10 {
        let variants = if run%2==0 {vec![("current",fetch_remote_range as Scheduler),("balanced",fetch_balanced_range as Scheduler)]}
                       else {vec![("balanced",fetch_balanced_range as Scheduler),("current",fetch_remote_range as Scheduler)]};
        for (name,fun) in variants {
            EVENTS.lock().unwrap().clear();
            let started=Instant::now();
            let bytes=fun(&backend,&object,mirage_types::CheckedRange::try_new(1234,4194372).unwrap()).unwrap();
            let elapsed=started.elapsed().as_secs_f64()*1000.0;
            assert_eq!(bytes.len(),4194372);
            let mut events=EVENTS.lock().unwrap().clone();events.sort_by_key(|e|e.0);
            if name=="current" {assert_eq!(events.len(),5);assert_eq!(events[4].1,68);assert!(events[4].2 >= events[..4].iter().map(|e|e.3).max().unwrap());}
            else {assert_eq!(events.len(),4);assert!(events.iter().all(|e|e.1==1048593));}
            if !first {println!(",");} first=false;
            print!("{{\"variant\":\"{}\",\"round\":{},\"ms\":{:.3},\"requests\":{},\"part_lengths\":{:?}}}",name,run,elapsed,events.len(),events.iter().map(|e|e.1).collect::<Vec<_>>());
        }
    }
    println!("]}}");
}
type Scheduler = fn(&Arc<dyn mirage_backend::ObjectBackend>,&mirage_backend::RemoteObjectRef,mirage_types::CheckedRange)->Result<Vec<u8>,FetchFailure>;
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rustc", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=Path("target/perf/research-20261012"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    source = (root / "crates/mirage-ffi/src/publisher.rs").read_text(encoding="utf-8")
    start = source.index("fn fetch_remote_range(\n")
    end = source.index("\n/// One `read_range`", start)
    original = source[start:end]
    assert "const SUBRANGE_BYTES: u64 = 1024 * 1024;" in source
    assert "const SUBRANGE_PARALLELISM: usize = 4;" in source
    balanced = original.replace("fn fetch_remote_range(", "fn fetch_balanced_range(", 1)
    balanced = balanced.replace("    let mut cursor = range.start();",
        "    let chunk_bytes = range.len().div_ceil(SUBRANGE_PARALLELISM as u64).max(SUBRANGE_BYTES);\n    let mut cursor = range.start();", 1)
    balanced = balanced.replace("SUBRANGE_BYTES.min(end - cursor)", "chunk_bytes.min(end - cursor)", 1)
    args.out.mkdir(parents=True, exist_ok=True)
    harness = args.out / "range_schedule.rs"
    harness.write_text(PREAMBLE + original + balanced + MAIN, encoding="utf-8")
    exe = args.out / "range_schedule.exe"
    subprocess.run([str(args.rustc), "--edition=2024", "-O", str(harness), "-o", str(exe)], check=True)
    completed = subprocess.run([str(exe.resolve())], check=True, capture_output=True, text=True)
    result = json.loads(completed.stdout)
    result["extracted_function_sha256"] = hashlib.sha256(original.encode()).hexdigest()
    result["harness_sha256"] = hashlib.sha256(harness.read_bytes()).hexdigest()
    result["caveat"] = "Artificial fixed backend delay, no HTTP, encryption or WinFsp; production scheduler is extracted verbatim."
    (args.out / "range-schedule.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    import statistics
    for variant in ["current", "balanced"]:
        print(variant, "median_ms", statistics.median(x["ms"] for x in result["samples"] if x["variant"] == variant))
    print("PASS:", result["assembly_cases_per_variant"], "byte-exact cases per variant, 20 timing runs")


if __name__ == "__main__":
    main()

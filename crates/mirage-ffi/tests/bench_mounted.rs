#![allow(unsafe_code)]

//! Mounted write-path benchmark: mounts a temporary managed volume through
//! the real WinFsp adapter, runs `D:\tmp\mirage-bench.ps1` against it, and
//! prints the measured output. Ignored by default — run explicitly with
//! `cargo test --release -p mirage-ffi --test bench_mounted -- --ignored`.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn adapter() -> std::path::PathBuf {
    if let Some(executable) = std::env::var_os("MIRAGE_FS_EXE") {
        let executable = std::path::PathBuf::from(executable);
        assert!(executable.is_file(), "MIRAGE_FS_EXE does not exist");
        return executable;
    }
    let executable = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/windows-msvc-debug/native/winfsp-adapter/Debug/mirage-fs.exe");
    assert!(executable.is_file(), "build the debug adapter first");
    executable
}

fn runtime_path() -> std::ffi::OsString {
    let program_files = std::env::var_os("ProgramFiles(x86)").expect("ProgramFiles(x86)");
    let runtime_bin = std::fs::read_dir(std::path::PathBuf::from(program_files).join("WinFsp/SxS"))
        .expect("installed WinFsp runtime")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.join("winfsp-x64.dll").is_file())
        .max()
        .expect("WinFsp x64 runtime");
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(runtime_bin).chain(std::env::split_paths(&inherited)))
        .expect("runtime PATH")
}

fn free_letter() -> char {
    ('R'..='Z')
        .rev()
        .find(|letter| {
            std::fs::metadata(format!("{letter}:\\"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        .expect("free test drive letter")
}

struct Host(std::process::Child);
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(stdin) = &mut self.0.stdin {
            let _ = stdin.write_all(b"STOP\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "mounts a real WinFsp volume and copies 512 MiB"]
fn mounted_write_benchmark() {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), b"seed").expect("seed");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "base.dat".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 2 * 1024 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: None,
    })
    .expect("import");
    let index = objects.path().join("mount.idx");
    mirage_index::compile_to_path(&imported.manifest, &index).expect("index");
    let state_root = tempfile::tempdir().expect("state root");
    let letter = free_letter();
    let mount_arg = format!("{letter}:");
    let mount_root = format!("{letter}:\\");
    let search_path = runtime_path();
    let host_log = std::fs::File::create(state_root.path().join("host-stderr.log")).expect("log");
    let mut host = Host(
        Command::new(adapter())
            .arg(&mount_arg)
            .arg(&index)
            .arg(state_root.path())
            .arg("S-1-1-0")
            .arg("--managed")
            .arg("68719476736") // advertised total: 64 GiB
            .arg("68719476736") // advertised free = dirty budget: 64 GiB
            .arg("--origin")
            .arg(objects.path())
            .env("PATH", &search_path)
            .stdout(Stdio::null())
            .stderr(Stdio::from(host_log))
            .spawn()
            .expect("spawn host"),
    );
    let ready_file = std::path::Path::new(&mount_root).join("base.dat");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready_file.exists() && Instant::now() < deadline {
        assert!(
            host.0.try_wait().expect("host status").is_none(),
            "host exited before the mount became ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready_file.exists(), "mount did not become ready");

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-File",
            "D:\\tmp\\mirage-bench.ps1",
            "-Target",
            &mount_root,
        ])
        .output()
        .expect("run benchmark");
    println!(
        "=== mirage-bench output ===\n{}\n===========================\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "benchmark script failed");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("hash_match    : True"),
        "content hash mismatch"
    );
    drop(host);
}

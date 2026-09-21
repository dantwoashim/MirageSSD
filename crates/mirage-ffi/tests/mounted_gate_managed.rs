#![cfg(windows)]
#![allow(unsafe_code)]

//! Mounted managed-volume gate: an ordinary program creates, writes,
//! flushes, renames, and deletes through WinFsp on a disposable mount, the
//! host is terminated, and a remount reads back the exact acknowledged
//! bytes. The managed volume is seeded from a committed index; mutations
//! live in the durable namespace + extent journal under the state root.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn free_letter() -> char {
    ('R'..='Z')
        .rev()
        .find(|letter| {
            std::fs::metadata(format!("{letter}:\\"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        .expect("free test drive letter")
}

fn adapter() -> std::path::PathBuf {
    // A prebuilt adapter (e.g. a Release development build) may be supplied
    // explicitly; the freshness check only applies to the default Debug
    // artifact.
    if let Some(executable) = std::env::var_os("MIRAGE_FS_EXE") {
        let executable = std::path::PathBuf::from(executable);
        assert!(executable.is_file(), "MIRAGE_FS_EXE does not exist");
        return executable;
    }
    let executable = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/windows-msvc-debug/native/winfsp-adapter/Debug/mirage-fs.exe");
    assert!(executable.is_file(), "build native adapter before Gate B");
    // A stale adapter cannot satisfy the gate: the binary must be newer than
    // every adapter source and the Rust FFI library it links.
    let adapter_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../native/winfsp-adapter");
    let built_at = std::fs::metadata(&executable)
        .and_then(|meta| meta.modified())
        .expect("adapter mtime");
    let mut stale_inputs = Vec::new();
    for dir in [adapter_root.join("src"), adapter_root.join("include")] {
        for entry in std::fs::read_dir(&dir).expect("adapter sources").flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext == "cpp" || ext == "hpp")
                && std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    > Some(built_at)
            {
                stale_inputs.push(path);
            }
        }
    }
    let rust_lib =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/mirage_ffi.lib");
    if let Ok(Ok(built)) = std::fs::metadata(&rust_lib).map(|meta| meta.modified())
        && built > built_at
    {
        stale_inputs.push(rust_lib);
    }
    assert!(
        stale_inputs.is_empty(),
        "adapter binary is older than its inputs; rebuild windows-msvc-debug: {stale_inputs:?}"
    );
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

fn wait_ready(host: &mut std::process::Child, file: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !file.exists() && Instant::now() < deadline {
        if let Some(status) = host.try_wait().expect("host status") {
            let stderr = host
                .stderr
                .take()
                .map(|mut stream| {
                    let mut text = String::new();
                    let _ = stream.read_to_string(&mut text);
                    text
                })
                .unwrap_or_default();
            panic!("host exited {status}: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(file.exists(), "managed mount did not become ready");
}

fn stop(host: &mut std::process::Child) {
    host.kill().expect("stop host");
    let _ = host.wait();
}

#[test]
fn managed_mount_writes_survive_restart() {
    // A committed base file exercises index-backed reads; local mutations
    // exercise the durable namespace + extent journal.
    let source = tempfile::tempdir().expect("source");
    let base_bytes: Vec<u8> = (0..65_536).map(|i| (i * 31 % 251) as u8).collect();
    std::fs::write(source.path().join("base.dat"), &base_bytes).expect("base file");
    // A seeded nested file exercises multi-component namespace resolution.
    let nested_dir = source.path().join("seeded");
    std::fs::create_dir(&nested_dir).expect("seeded dir");
    let inner_bytes: Vec<u8> = (0..20_000).map(|i| (i * 17 % 211) as u8).collect();
    std::fs::write(nested_dir.join("inner.dat"), &inner_bytes).expect("inner file");
    let nested_dir = source.path().join("seeded").join("deep");
    std::fs::create_dir(&nested_dir).expect("deep dir");
    let deep_bytes: Vec<u8> = (0..9_000).map(|i| (i * 13 % 197) as u8).collect();
    std::fs::write(nested_dir.join("leaf.dat"), &deep_bytes).expect("deep file");
    std::fs::write(source.path().join("doomed-root.dat"), b"doomed").expect("doomed seed");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![
            PlannedFile {
                relative_path: "base.dat".into(),
                class: FileClass::VirtualContainer,
            },
            PlannedFile {
                relative_path: "seeded/inner.dat".into(),
                class: FileClass::VirtualContainer,
            },
            PlannedFile {
                relative_path: "seeded/deep/leaf.dat".into(),
                class: FileClass::VirtualContainer,
            },
            PlannedFile {
                relative_path: "doomed-root.dat".into(),
                class: FileClass::VirtualContainer,
            },
        ],
        page_size: 64 * 1024,
        pack_target: 2 * 1024 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: None,
    })
    .expect("import");
    let index = objects.path().join("mount.idx");
    mirage_index::compile_to_path(&imported.manifest, &index).expect("index");
    let state_root = tempfile::tempdir().expect("state root");
    let mount_letter = free_letter();
    let mount_arg = format!("{mount_letter}:");
    let mount = std::path::PathBuf::from(format!("{mount_letter}:\\"));
    let search_path = runtime_path();
    let executable = adapter();
    let host_log =
        std::fs::File::create(state_root.path().join("host-stderr.log")).expect("host log");

    let mut host = Command::new(&executable)
        .arg(&mount_arg)
        .arg(&index)
        .arg(state_root.path())
        .arg("S-1-1-0")
        .arg("--managed")
        .arg("536870912")
        .arg("65536")
        .arg("--origin")
        .arg(objects.path())
        .env("PATH", &search_path)
        .stdout(Stdio::null())
        .stderr(Stdio::from(host_log.try_clone().unwrap()))
        .spawn()
        .expect("start managed host");
    wait_ready(&mut host, &mount.join("base.dat"));

    // Committed base content reads through the object mirror.
    let mut base = Vec::new();
    std::fs::File::open(mount.join("base.dat"))
        .expect("open base")
        .read_to_end(&mut base)
        .expect("read base");
    assert_eq!(base, base_bytes);

    // Create + write + flush a new file entirely offline.
    let created = mount.join("notes.txt");
    let payload: Vec<u8> = (0..40_000).map(|i| (i % 97) as u8).collect();
    {
        let mut file = std::fs::File::create(&created).expect("create");
        file.write_all(&payload).expect("write");
        file.sync_all().expect("flush");
    }
    let mut readback = Vec::new();
    std::fs::File::open(&created)
        .expect("reopen")
        .read_to_end(&mut readback)
        .expect("read back");
    assert_eq!(readback, payload);

    // Partial overwrite keeps untouched bytes.
    {
        use std::io::{Seek, SeekFrom};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&created)
            .expect("overwrite open");
        file.seek(SeekFrom::Start(100)).expect("seek");
        file.write_all(b"PATCHED").expect("patch write");
        file.sync_all().expect("patch flush");
    }
    let mut expected = payload.clone();
    expected[100..107].copy_from_slice(b"PATCHED");
    let mut patched = Vec::new();
    std::fs::File::open(&created)
        .expect("patched open")
        .read_to_end(&mut patched)
        .expect("patched read");
    assert_eq!(patched, expected);

    // Rename keeps content; delete removes the entry.
    let renamed = mount.join("renamed.txt");
    std::fs::rename(&created, &renamed).expect("rename");
    assert!(!created.exists());
    assert!(std::fs::metadata(&renamed).expect("renamed").len() == expected.len() as u64);
    let doomed = mount.join("doomed.txt");
    std::fs::write(&doomed, b"gone").expect("doomed write");
    std::fs::remove_file(&doomed).expect("doomed delete");
    assert!(!doomed.exists());

    // PowerShell Remove-Item exercises the SetBasicInfo + delete path that
    // real shells drive.
    let ps_doomed = mount.join("ps-doomed.txt");
    std::fs::write(&ps_doomed, b"gone").expect("ps doomed write");
    let ps_status = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(format!(
            "Remove-Item -LiteralPath '{}' -Force",
            ps_doomed.display()
        ))
        .env("PATH", &search_path)
        .status()
        .expect("powershell remove");
    assert!(ps_status.success(), "powershell Remove-Item failed");
    assert!(!ps_doomed.exists());

    // A directory created through the mount appears in enumeration.
    std::fs::create_dir(mount.join("newdir")).expect("mkdir");
    let names: Vec<String> = std::fs::read_dir(&mount)
        .expect("enumerate")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|name| name == "newdir"));
    assert!(names.iter().any(|name| name == "base.dat"));
    assert!(names.iter().any(|name| name == "renamed.txt"));

    // Nested namespace operations: mkdir d1, mkdir d1\d2, create+write+
    // readback d1\d2\f.txt, enumerate both levels, rename, delete, rmdir.
    std::fs::create_dir(mount.join("d1")).expect("mkdir d1");
    std::fs::create_dir(mount.join("d1").join("d2")).expect("mkdir d1/d2");
    let nested_file = mount.join("d1").join("d2").join("f.txt");
    let nested_payload = b"nested-payload";
    std::fs::write(&nested_file, nested_payload).expect("nested create+write");
    let mut nested_read = Vec::new();
    std::fs::File::open(&nested_file)
        .expect("nested open")
        .read_to_end(&mut nested_read)
        .expect("nested read");
    assert_eq!(nested_read, nested_payload);
    let d1_names: Vec<String> = std::fs::read_dir(mount.join("d1"))
        .expect("listdir d1")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(d1_names.iter().any(|name| name == "d2"));
    let d2_names: Vec<String> = std::fs::read_dir(mount.join("d1").join("d2"))
        .expect("listdir d2")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(d2_names.iter().any(|name| name == "f.txt"));
    let renamed_nested = mount.join("d1").join("g.txt");
    std::fs::rename(&nested_file, &renamed_nested).expect("nested rename");
    assert!(!nested_file.exists());
    let mut renamed_nested_read = Vec::new();
    std::fs::File::open(&renamed_nested)
        .expect("renamed nested open")
        .read_to_end(&mut renamed_nested_read)
        .expect("renamed nested read");
    assert_eq!(renamed_nested_read, nested_payload);
    // Forward-slash separators resolve identically.
    let mut slash_read = Vec::new();
    std::fs::File::open(format!("{mount_letter}:/d1/g.txt"))
        .expect("slash-path open")
        .read_to_end(&mut slash_read)
        .expect("slash-path read");
    assert_eq!(slash_read, nested_payload);
    std::fs::remove_file(&renamed_nested).expect("nested delete");
    assert!(!renamed_nested.exists());
    std::fs::remove_dir(mount.join("d1").join("d2")).expect("rmdir d2");
    assert!(!mount.join("d1").join("d2").exists());

    // Seeded nested files read through the namespace's legacy binding.
    let mut inner = Vec::new();
    std::fs::File::open(mount.join("seeded").join("inner.dat"))
        .expect("seeded nested open")
        .read_to_end(&mut inner)
        .expect("seeded nested read");
    assert_eq!(inner, inner_bytes);
    let mut leaf = Vec::new();
    std::fs::File::open(format!("{mount_letter}:/seeded/deep/leaf.dat"))
        .expect("seeded deep open")
        .read_to_end(&mut leaf)
        .expect("seeded deep read");
    assert_eq!(leaf, deep_bytes);

    // Deletes of seeded files — root and nested — must be durable.
    std::fs::remove_file(mount.join("doomed-root.dat")).expect("delete seeded root");
    std::fs::remove_file(mount.join("seeded").join("inner.dat")).expect("delete seeded nested");
    assert!(!mount.join("doomed-root.dat").exists());

    // The dirty-payload budget is 64 KiB: a write beyond it must surface the
    // OS disk-full error, not corrupt or wedge.
    let oversized = vec![0u8; 128 * 1024];
    let error = std::fs::write(mount.join("too-big.bin"), &oversized)
        .expect_err("over-budget write must fail");
    assert_eq!(
        error.raw_os_error(),
        Some(112),
        "expected ERROR_DISK_FULL, got {error}"
    );
    // Earlier content is untouched by the refused write.
    let mut intact = Vec::new();
    std::fs::File::open(&renamed)
        .expect("intact open")
        .read_to_end(&mut intact)
        .expect("intact read");
    assert_eq!(intact, expected);

    stop(&mut host);

    // Remount: the durable namespace + extent journal replay the state.
    let mut second = Command::new(&executable)
        .arg(&mount_arg)
        .arg(&index)
        .arg(state_root.path())
        .arg("S-1-1-0")
        .arg("--managed")
        .arg("536870912")
        .arg("65536")
        .arg("--origin")
        .arg(objects.path())
        .env("PATH", &search_path)
        .stdout(Stdio::null())
        .stderr(Stdio::from(host_log.try_clone().unwrap()))
        .spawn()
        .expect("remount managed host");
    wait_ready(&mut second, &mount.join("renamed.txt"));

    let mut after = Vec::new();
    std::fs::File::open(&renamed)
        .expect("remount open")
        .read_to_end(&mut after)
        .expect("remount read");
    assert_eq!(after, expected, "acknowledged bytes must survive restart");
    assert!(!created.exists());
    assert!(!doomed.exists());
    assert!(mount.join("newdir").is_dir());
    // Seeded deletions are durable; the nested tree and its siblings stay.
    assert!(!mount.join("doomed-root.dat").exists());
    let seeded_names: Vec<String> = std::fs::read_dir(mount.join("seeded"))
        .expect("remount seeded listdir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!seeded_names.iter().any(|name| name == "inner.dat"));
    assert!(seeded_names.iter().any(|name| name == "deep"));
    let mut leaf_after = Vec::new();
    std::fs::File::open(mount.join("seeded").join("deep").join("leaf.dat"))
        .expect("remount deep open")
        .read_to_end(&mut leaf_after)
        .expect("remount deep read");
    assert_eq!(leaf_after, deep_bytes);
    assert!(mount.join("d1").is_dir());
    let mut base_after = Vec::new();
    std::fs::File::open(mount.join("base.dat"))
        .expect("remount base")
        .read_to_end(&mut base_after)
        .expect("remount base read");
    assert_eq!(base_after, base_bytes);
    stop(&mut second);

    // Quiesce compaction ran at stop: every remaining journal payload is
    // referenced by a live extent row — no orphans accumulate.
    let volume = RepositoryId::from_bytes([9; 16]);
    let db = mirage_db::Database::open(&state_root.path().join("control.db")).expect("control db");
    let referenced = db.referenced_payload_ids(volume).expect("referenced");
    let mut on_disk: Vec<String> = std::fs::read_dir(state_root.path().join("journal"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".payload"))
                .collect()
        })
        .unwrap_or_default();
    on_disk.sort();
    let mut expected_names: Vec<String> = referenced
        .iter()
        .map(|id| {
            let mut name = String::new();
            for byte in id {
                name.push_str(&format!("{byte:02x}"));
            }
            name.push_str(".payload");
            name
        })
        .collect();
    expected_names.sort();
    assert_eq!(on_disk, expected_names, "journal payloads != live extents");
}

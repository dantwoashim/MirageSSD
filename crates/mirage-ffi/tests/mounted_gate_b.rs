#![cfg(windows)]
#![allow(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

#[test]
fn mounted_provider_matches_one_hundred_thousand_random_bytes() {
    let source = tempfile::tempdir().expect("source");
    let mount_letter = ('R'..='Z')
        .rev()
        .find(|letter| {
            std::fs::metadata(format!("{letter}:\\"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        .expect("free test drive letter");
    let mount_arg = format!("{mount_letter}:");
    let mount = std::path::PathBuf::from(format!("{mount_letter}:\\"));
    let objects = tempfile::tempdir().expect("objects");
    std::fs::create_dir(source.path().join("assets")).expect("directory");
    let expected: Vec<u8> = (0..1_048_576)
        .map(|i| ((i * 131 + 17) % 251) as u8)
        .collect();
    std::fs::write(source.path().join("assets/data.pak"), &expected).expect("source file");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([7; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "assets/data.pak".into(),
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
    let executable = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/windows-msvc-debug/native/winfsp-adapter/Debug/mirage-fs.exe");
    assert!(executable.is_file(), "build native adapter before Gate B");
    let program_files = std::env::var_os("ProgramFiles(x86)").expect("ProgramFiles(x86)");
    let runtime_bin = std::fs::read_dir(std::path::PathBuf::from(program_files).join("WinFsp/SxS"))
        .expect("installed WinFsp runtime")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.join("winfsp-x64.dll").is_file())
        .max()
        .expect("WinFsp x64 runtime");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let search_path = std::env::join_paths(
        std::iter::once(runtime_bin).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("runtime PATH");
    let mut host = Command::new(&executable)
        .arg(&mount_arg)
        .arg(&index)
        .arg(objects.path())
        .arg("S-1-1-0")
        .env("PATH", &search_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start host");
    let mounted_file = mount.join("assets/data.pak");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mounted_file.is_file() && Instant::now() < deadline {
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
    if !mounted_file.is_file() {
        let listing = std::fs::read_dir(&mount)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>()
            })
            .map_err(|error| error.to_string());
        let alive = host.try_wait().expect("host status");
        host.kill().expect("stop failed host");
        let _ = host.wait();
        let stderr = host
            .stderr
            .take()
            .map(|mut stream| {
                let mut text = String::new();
                let _ = stream.read_to_string(&mut text);
                text
            })
            .unwrap_or_default();
        panic!(
            "mounted namespace did not become ready: mount_exists={} listing={listing:?} host={alive:?} stderr={stderr}",
            mount.exists()
        );
    }
    let mut file = std::fs::File::open(&mounted_file).expect("mounted file");
    let mut state = 0x4d595df4d0f33173_u64;
    let mut byte = [0_u8; 1];
    let mut latencies = Vec::with_capacity(100_000);
    for _ in 0..100_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let offset = (state as usize) % expected.len();
        let started = Instant::now();
        file.seek(SeekFrom::Start(offset as u64)).expect("seek");
        file.read_exact(&mut byte).expect("read");
        latencies.push(started.elapsed().as_nanos() as u64);
        assert_eq!(byte[0], expected[offset], "difference at {offset}");
    }
    latencies.sort_unstable();
    let p99_ns = latencies[latencies.len() * 99 / 100];
    println!("gate_b_local_hit_p99_ns={p99_ns}");
    assert!(
        p99_ns < 5_000_000,
        "local-hit p99 exceeds provisional 5ms budget"
    );
    let second = std::fs::File::open(&mounted_file).expect("shared read handle");
    assert_eq!(
        second.metadata().expect("shared metadata").len(),
        expected.len() as u64
    );
    let direct = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED,
        )
        .open(&mounted_file)
        .expect("non-cached overlapped open");
    let layout = std::alloc::Layout::from_size_align(4096, 4096).expect("aligned layout");
    let aligned = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!aligned.is_null());
    let aligned_slice = unsafe { std::slice::from_raw_parts_mut(aligned, 4096) };
    assert_eq!(
        direct
            .seek_read(aligned_slice, 0)
            .expect("non-cached overlapped read"),
        4096
    );
    assert_eq!(aligned_slice, &expected[..4096]);
    unsafe {
        std::alloc::dealloc(aligned, layout);
    }
    host.kill().expect("stop host");
    let _ = host.wait();

    let mut async_host = Command::new(&executable)
        .arg(&mount_arg)
        .arg(&index)
        .arg(objects.path())
        .arg("S-1-1-0")
        .env("PATH", &search_path)
        .env("MIRAGE_TEST_ASYNC_DELAY_MS", "100")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start async host");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mounted_file.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(mounted_file.is_file(), "async mount did not become ready");
    let async_path = mounted_file.clone();
    let reader = std::thread::spawn(move || {
        let mut file = std::fs::File::open(async_path).expect("async open");
        let mut bytes = vec![0; 4096];
        file.read_exact(&mut bytes).expect("async read");
        bytes
    });
    std::thread::sleep(Duration::from_millis(10));
    let metadata_started = Instant::now();
    std::fs::metadata(&mount).expect("metadata remains responsive");
    assert!(metadata_started.elapsed() < Duration::from_millis(75));
    assert_eq!(reader.join().expect("reader thread"), expected[..4096]);
    let mapped_file = std::fs::File::open(&mounted_file).expect("mapped open");
    let mapped =
        unsafe { memmap2::MmapOptions::new().map(&mapped_file) }.expect("mapped delayed pages");
    assert_eq!(blake3::hash(&mapped), blake3::hash(&expected));
    async_host.kill().expect("stop async host");
    let _ = async_host.wait();
}

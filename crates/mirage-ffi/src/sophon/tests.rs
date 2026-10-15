use super::*;
use serde_json::json;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixture(root: &Path, seed: bool, memory: usize, budget: u64) -> (PathBuf, Vec<u8>) {
    let bytes: Vec<u8> = (0..9000).map(|i| (i * 17) as u8).collect();
    let chunks: Vec<_> = bytes
        .chunks(4096)
        .enumerate()
        .map(|(i, b)| {
            json!({
                "name": format!("chunk_{i}"), "md5":format!("{:x}", Md5::digest(b)),
                "offset":i*4096,"size":zstd::stream::encode_all(b,1).unwrap().len(),"dsize":b.len()
            })
        })
        .collect();
    let manifest = root.join("manifest.json");
    std::fs::write(&manifest, serde_json::to_vec(&json!({"chunk_url_prefix":"https://fixture.invalid/chunks",
        "assets":[{"name":"Data/Mixed.blk","size":bytes.len(),"md5":format!("{:x}",Md5::digest(&bytes)),"chunks":chunks}]})).unwrap()).unwrap();
    let seeds = root.join("seeds");
    std::fs::create_dir_all(seeds.join("Data")).unwrap();
    if seed {
        std::fs::write(seeds.join("Data/Mixed.blk"), &bytes).unwrap();
    }
    let config = root.join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({"manifests":[manifest],"export_prefixes":["Data/"],
        "seed_roots":[seeds],"cache_root":root.join("owned"),"disk_budget_bytes":budget,
        "disk_free_floor_bytes":0,"memory_budget_bytes":memory}))
        .unwrap(),
    )
    .unwrap();
    (config, bytes)
}
fn index(source: &SophonSource) -> u64 {
    source.entries[&"data\\mixed.blk".encode_utf16().collect::<Vec<_>>()].index
}

#[cfg(windows)]
#[test]
fn failed_corrupt_cache_removal_serves_verified_remote_bytes() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), false, 0, 8192);
    let mut source = SophonSource::open(&config).unwrap();
    let (requests, server) = origin(&mut source, bytes.clone(), 2, false);
    let mut output = vec![0; 4096];
    source.read(index(&source), 0, &mut output).unwrap();
    assert_eq!(output, bytes[..4096]);
    let cache = std::fs::read_dir(source.config.cache_root.join("chunks-v1"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "chunk")
        })
        .unwrap()
        .path();
    std::fs::write(&cache, b"corrupt").unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&cache)
        .unwrap();
    output.fill(0);
    source.read(index(&source), 0, &mut output).unwrap();
    assert_eq!(output, bytes[..4096]);
    assert_eq!(source.statistics().cache_failures, 1);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    drop(locked);
    server.join().unwrap();
}
fn origin(
    source: &mut SophonSource,
    bytes: Vec<u8>,
    requests: usize,
    bad_first: bool,
) -> (Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let prefix = format!("http://{}/chunks", listener.local_addr().unwrap());
    for node in &mut source.nodes {
        if let Some(a) = &mut node.asset {
            a.prefix = prefix.clone();
        }
    }
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let job = std::thread::spawn(move || {
        for _ in 0..requests {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let length = socket.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let path = request.split_whitespace().nth(1).unwrap();
            let i = path.rsplit('_').next().unwrap().parse::<usize>().unwrap();
            let plain = &bytes[i * 4096..((i + 1) * 4096).min(bytes.len())];
            let mut body = zstd::stream::encode_all(plain, 1).unwrap();
            let number = seen.fetch_add(1, Ordering::SeqCst);
            if bad_first && number == 0 {
                body[0] ^= 0x40;
            }
            std::thread::sleep(Duration::from_millis(30));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(&body).unwrap();
        }
    });
    (count, job)
}
#[test]
fn seed_ranges_cross_chunks_eof_and_casefold_enumeration_without_duplication() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), true, 8192, 8192);
    let source = SophonSource::open(&config).unwrap();
    // All remote slots occupied: resident seed reads still make progress.
    *source.owners.lock().unwrap() = 8;
    let mut out = vec![0; 5000];
    assert_eq!(source.read(index(&source), 3999, &mut out).unwrap(), 5000);
    assert_eq!(out, &bytes[3999..8999]);
    assert_eq!(source.read(index(&source), 9000, &mut out).unwrap(), 0);
    assert_eq!(source.read(index(&source), u64::MAX, &mut out).unwrap(), 0);
    assert_eq!(source.children(1, None, 20).unwrap()[0].0, "Mixed.blk");
    assert!(
        source
            .children(1, Some("MIXED.BLK"), 20)
            .unwrap()
            .is_empty()
    );
    assert_eq!(source.statistics().cdn_chunks, 0);
    assert_eq!(source.statistics().seed_hits, 3);
    assert_eq!(source.statistics().managed_disk_bytes, 0);
    assert!(source.statistics().memory_bytes <= 8192);
    assert!(matches!(
        SophonSource::open(&config),
        Err(MirageStatus::Conflict)
    ));
    assert_eq!(
        std::fs::read(root.path().join("seeds/Data/Mixed.blk")).unwrap(),
        bytes
    );
}
#[test]
fn shared_cold_miss_corruption_retry_and_disk_restart_are_byte_correct() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), false, 8192, 8192);
    let mut source = SophonSource::open(&config).unwrap();
    let (count, server) = origin(&mut source, bytes.clone(), 2, true);
    let i = index(&source);
    let mut first = [0u8; 100];
    assert_eq!(
        source.read(i, 0, &mut first),
        Err(MirageStatus::IntegrityFailure)
    );
    assert_eq!(source.statistics().managed_disk_bytes, 0);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..16)
            .map(|_| {
                scope.spawn(|| {
                    let mut out = [0u8; 100];
                    assert_eq!(source.read(i, 71, &mut out).unwrap(), 100);
                    assert_eq!(&out, &bytes[71..171]);
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap();
        }
    });
    server.join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(source.statistics().cdn_chunks, 1);
    assert_eq!(source.statistics().managed_disk_bytes, 4096);
    // Match the same immutable representation in a fresh source process.
    let prefix = source.nodes[i as usize]
        .asset
        .as_ref()
        .unwrap()
        .prefix
        .clone();
    drop(source);
    let mut source = SophonSource::open(&config).unwrap();
    source.nodes[i as usize].asset.as_mut().unwrap().prefix = prefix;
    let mut out = [0; 100];
    assert_eq!(source.read(i, 71, &mut out).unwrap(), 100);
    assert_eq!(&out, &bytes[71..171]);
    assert_eq!(source.statistics().disk_hits, 1);
}
#[test]
fn stale_seed_is_rejected_and_disk_budget_evicts_owned_chunks_only() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), true, 0, 4096);
    std::fs::write(root.path().join("seeds/Data/Mixed.blk"), vec![0; 9000]).unwrap();
    let mut source = SophonSource::open(&config).unwrap();
    let (_, server) = origin(&mut source, bytes.clone(), 3, false);
    let mut out = vec![0; 9000];
    assert_eq!(source.read(index(&source), 0, &mut out).unwrap(), 9000);
    assert_eq!(out, bytes);
    server.join().unwrap();
    assert_eq!(source.statistics().seed_hits, 0);
    assert!(source.statistics().managed_disk_bytes <= 4096);
    assert_eq!(
        std::fs::read(root.path().join("seeds/Data/Mixed.blk")).unwrap(),
        vec![0; 9000]
    );
}
#[test]
fn malformed_generation_is_refused_before_namespace_or_network() {
    let root = tempfile::tempdir().unwrap();
    let (config, _) = fixture(root.path(), false, 0, 0);
    let path = root.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["assets"][0]["chunks"][1]["offset"] = json!(4097);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(matches!(
        SophonSource::open(&config),
        Err(MirageStatus::IntegrityFailure)
    ));
    manifest["assets"][0]["chunks"][1]["offset"] = json!(4096);
    manifest["assets"][0]["name"] = json!("Data/../escape");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(matches!(
        SophonSource::open(&config),
        Err(MirageStatus::IntegrityFailure)
    ));
}

#[test]
fn cleanup_excludes_live_owner_and_preserves_seeds_unknown_files_and_balanced_pins() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), true, 0, 8192);
    let source = SophonSource::open(&config).unwrap();
    let owned = root.path().join("owned");
    let a = [1; 32];
    let b = [2; 32];
    source
        .disk
        .lock()
        .unwrap()
        .put(a, &bytes[..4096], 0)
        .unwrap();
    source
        .disk
        .lock()
        .unwrap()
        .put(b, &bytes[..4096], 0)
        .unwrap();
    std::fs::write(owned.join("chunks-v1/user-note.txt"), b"preserve").unwrap();
    assert!(matches!(
        retention::cleanup(&owned, Vec::new()),
        Err(MirageStatus::Conflict)
    ));
    drop(source);
    let balanced = retention::cleanup(&owned, vec![a]).unwrap();
    assert!(!balanced.pending);
    assert_eq!(balanced.reclaimed_bytes, 4096);
    assert_eq!(balanced.retained_bytes, 4096);
    let disposable = retention::cleanup(&owned, Vec::new()).unwrap();
    assert!(!disposable.pending);
    assert_eq!(disposable.reclaimed_bytes, 4096);
    assert_eq!(disposable.retained_bytes, 0);
    assert_eq!(
        std::fs::read(root.path().join("seeds/Data/Mixed.blk")).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read(owned.join("chunks-v1/user-note.txt")).unwrap(),
        b"preserve"
    );
}

#[test]
fn pins_bound_placement_and_shrinking_reports_unmet_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut disk = cache::DiskCache::open(root.path(), 4096).unwrap();
    disk.put([1; 32], &[1; 4096], 0).unwrap();
    disk.pin([1; 32]);
    disk.put([2; 32], &[2; 4096], 0).unwrap();
    assert_eq!(disk.used(), 4096);
    assert_eq!(disk.retained_bytes(), 4096);
    assert_eq!(disk.set_budget(4095), Err(MirageStatus::DiskFull));
}

#[test]
fn native_materialization_reuses_verified_originals_and_never_publishes_bad_whole_file() {
    let root = tempfile::tempdir().unwrap();
    let (_, bytes) = fixture(root.path(), true, 0, 0);
    let path = root.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["assets"][0]["name"] = json!("GenshinImpact.exe");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(root.path().join("seeds/GenshinImpact.exe"), &bytes).unwrap();
    let generation = publisher::Generation {
        version: "1.2.3".into(),
        build_id: "fixture".into(),
        language: "en-us".into(),
        manifests: vec![path.clone()],
        logical_bytes: bytes.len() as u64,
    };
    let native = root.path().join("native");
    let result = install::materialize_native(
        &generation,
        &native,
        vec![root.path().join("seeds")],
        &root.path().join("acquire"),
    )
    .unwrap();
    assert_eq!(result.native_bytes, bytes.len() as u64);
    assert_eq!(
        std::fs::read(native.join("GenshinImpact.exe")).unwrap(),
        bytes
    );
    assert!(native.join("config.ini").is_file());
    manifest["assets"][0]["md5"] = json!("00000000000000000000000000000000");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let bad = root.path().join("bad-native");
    assert!(matches!(
        install::materialize_native(
            &generation,
            &bad,
            vec![root.path().join("seeds")],
            &root.path().join("bad-acquire")
        ),
        Err(MirageStatus::IntegrityFailure)
    ));
    assert!(!bad.join("config.ini").exists());
    assert_eq!(
        std::fs::read(root.path().join("seeds/GenshinImpact.exe")).unwrap(),
        bytes
    );
}

#[test]
fn learning_excludes_launcher_scans_and_prepares_only_the_budgeted_generation() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), true, 0, 8192);
    let source = SophonSource::open(&config).unwrap();
    let i = index(&source);
    let mut out = [0; 100];
    source.read(i, 0, &mut out).unwrap();
    assert_eq!(source.statistics().learned_chunks, 0);
    source.set_gameplay_learning(true).unwrap();
    source.read(i, 0, &mut out).unwrap();
    source.read(i, 4096, &mut out).unwrap();
    source.set_gameplay_learning(false).unwrap();
    assert_eq!(source.statistics().learned_chunks, 2);
    assert_eq!(source.prepare_learned(4096).unwrap().len(), 1);
    assert!(matches!(
        source.prepare_learned(8193),
        Err(MirageStatus::InvalidArgument)
    ));
    drop(source);
    let source = SophonSource::open(&config).unwrap();
    assert_eq!(source.statistics().learned_chunks, 2);
    drop(source);
    let path = root.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["chunk_url_prefix"] = json!("https://fixture.invalid/new-generation");
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let source = SophonSource::open(&config).unwrap();
    assert_eq!(source.statistics().learned_chunks, 0);
    assert_eq!(
        std::fs::read(root.path().join("seeds/Data/Mixed.blk")).unwrap(),
        bytes
    );
}

#[cfg(windows)]
#[test]
fn locked_cleanup_stays_pending_and_recovery_precedes_next_mount() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = tempfile::tempdir().unwrap();
    let (config, _) = fixture(root.path(), true, 0, 8192);
    let source = SophonSource::open(&config).unwrap();
    source
        .disk
        .lock()
        .unwrap()
        .put([1; 32], &[1; 4096], 0)
        .unwrap();
    drop(source);
    let owned = root.path().join("owned");
    let chunk = std::fs::read_dir(owned.join("chunks-v1"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(chunk)
        .unwrap();
    let cleanup = retention::cleanup(&owned, Vec::new()).unwrap();
    assert!(cleanup.pending);
    assert_eq!(cleanup.reclaimed_bytes, 0);
    assert!(matches!(
        SophonSource::open(&config),
        Err(MirageStatus::IoError)
    ));
    drop(held);
    let source = SophonSource::open(&config).unwrap();
    assert_eq!(source.statistics().managed_disk_bytes, 0);
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(owned.join("cleanup.json")).unwrap()).unwrap();
    assert_eq!(journal["pending"], false);
    assert_eq!(journal["reclaimed_bytes"], 4096);
}

#[test]
fn native_ffi_lookup_read_stat_and_enumeration_use_the_source() {
    let root = tempfile::tempdir().unwrap();
    let (config, bytes) = fixture(root.path(), true, 8192, 8192);
    let path: Vec<_> = config.to_string_lossy().encode_utf16().collect();
    let mut engine = std::ptr::null_mut();
    // SAFETY: buffers are live for their explicit lengths; output pointers
    // are closed/destroyed before their backing fixture disappears.
    unsafe {
        assert_eq!(
            crate::engine_create::mirage_engine_create_sophon(
                path.as_ptr(),
                path.len(),
                &mut engine
            ),
            MirageStatus::Ok
        );
        let name: Vec<_> = "\\DATA\\mixed.BLK".encode_utf16().collect();
        let mut file = std::ptr::null_mut();
        assert_eq!(
            crate::mirage_lookup(engine, name.as_ptr(), name.len(), &mut file),
            MirageStatus::Ok
        );
        let mut out = vec![0; 5000];
        let mut count = usize::MAX;
        assert_eq!(
            crate::mirage_read(file, 3999, out.as_mut_ptr(), out.len(), &mut count),
            MirageStatus::Ok
        );
        assert_eq!(count, 5000);
        assert_eq!(out, &bytes[3999..8999]);
        assert_eq!(
            crate::mirage_write(file, 0, out.as_ptr(), 1, &mut count),
            MirageStatus::AccessDenied
        );
        assert_eq!(crate::mirage_file_close(file), MirageStatus::Ok);
        assert_eq!(crate::mirage_engine_destroy(engine), MirageStatus::Ok);
    }
}

#[cfg(windows)]
#[test]
fn mounted_source_reads_exact_bytes_and_remounts_without_network() {
    use std::process::{Command, Stdio};
    struct Host(std::process::Child);
    impl Drop for Host {
        fn drop(&mut self) {
            if let Some(input) = &mut self.0.stdin {
                let _ = writeln!(input, "STOP");
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (config, expected) = fixture(root.path(), true, 8192, 8192);
    let mount = root.path().join("mount");
    let exe = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/windows-msvc-debug/native/winfsp-adapter/Debug/mirage-fs.exe");
    assert!(
        exe.is_file(),
        "rebuild the debug native adapter before workspace tests"
    );
    let runtime = std::fs::read_dir(
        PathBuf::from(std::env::var_os("ProgramFiles(x86)").unwrap()).join("WinFsp/SxS"),
    )
    .unwrap()
    .filter_map(Result::ok)
    .map(|entry| entry.path().join("bin"))
    .filter(|path| path.join("winfsp-x64.dll").is_file())
    .max()
    .unwrap();
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let search =
        std::env::join_paths(std::iter::once(runtime).chain(std::env::split_paths(&inherited)))
            .unwrap();
    for _ in 0..2 {
        let mut host = Host(
            Command::new(&exe)
                .arg(&mount)
                .arg(&config)
                .arg(root.path())
                .arg("S-1-1-0")
                .args(["--sophon", "104857600", "52428800"])
                .env("PATH", &search)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let path = mount.join("Data/Mixed.blk");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.is_file() {
            assert!(
                host.0.try_wait().unwrap().is_none(),
                "native source host exited"
            );
            assert!(
                Instant::now() < deadline,
                "native source mount did not become ready"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(std::fs::write(&path, b"must refuse").is_err());
        assert_eq!(std::fs::read_dir(mount.join("Data")).unwrap().count(), 1);
        drop(host);
    }
}

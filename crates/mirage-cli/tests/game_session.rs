#![cfg(windows)]
#![allow(unsafe_code)]
//! Real WinFsp + synthetic executable session. Proves controller handoff and
//! cache lifecycle; does not constitute publisher title/gameplay acceptance.
use md5::{Digest, Md5};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn detached_game_handoff_outlives_launcher_and_cleans_only_owned_chunks() {
    run_session(false, false, false);
}
#[test]
fn monitoring_failure_preserves_mount_through_detached_game_exit() {
    run_session(true, false, false);
}
#[test]
fn crashed_worker_preserves_game_source_then_next_worker_recovers_cleanup() {
    run_session(false, true, false);
}
#[test]
fn short_path_root_matches_junction_and_process_identity() {
    run_session(false, false, true);
}
fn run_session(fail_monitor: bool, crash_worker: bool, short_root: bool) {
    let temporary = tempfile::Builder::new()
        .prefix("mirage-audit-long-directory-")
        .tempdir()
        .unwrap();
    let base = if short_root {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide: Vec<_> = temporary
            .path()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut output = vec![0u16; 32768];
        let length =
            unsafe { GetShortPathNameW(wide.as_ptr(), output.as_mut_ptr(), output.len() as u32) };
        assert!(length > 0 && length < output.len() as u32);
        PathBuf::from(String::from_utf16(&output[..length as usize]).unwrap())
    } else {
        temporary.path().to_path_buf()
    };
    let root = base.join("managed");
    let active = root.join("generations/fixture");
    let native = active.join("native");
    let bundle = base.join("bundle");
    fs::create_dir_all(&native).unwrap();
    if crash_worker {
        fs::write(
            native.join("fixture-hold-child.txt"),
            b"bounded post-read hold",
        )
        .unwrap();
    }
    fs::create_dir(&bundle).unwrap();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let adapter = repository.join("build/windows-msvc-debug/native/winfsp-adapter/Debug");
    assert!(
        adapter.join("mirage-game-fixture.exe").is_file(),
        "rebuild windows-msvc-debug before controller fixture tests"
    );
    fs::copy(
        adapter.join("mirage-game-fixture.exe"),
        native.join("GenshinImpact.exe"),
    )
    .unwrap();
    fs::copy(adapter.join("mirage-fs.exe"), bundle.join("mirage-fs.exe")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_mirage"), bundle.join("mirage.exe")).unwrap();
    let seeds = base.join("originals");
    let name = "GenshinImpact_Data/StreamingAssets/AudioAssets/fixture.bnk";
    let bytes = b"verified synthetic publisher fixture";
    let md5 = format!("{:x}", Md5::digest(bytes));
    let names = [
        name,
        "GenshinImpact_Data/StreamingAssets/AssetBundles/blocks/fixture.blk",
        "GenshinImpact_Data/StreamingAssets/VideoAssets/fixture.usm",
    ];
    let mut assets = Vec::new();
    for name in names {
        fs::create_dir_all(seeds.join(name).parent().unwrap()).unwrap();
        fs::write(seeds.join(name), bytes).unwrap();
        assets.push(json!({"name":name,"size":bytes.len(),"md5":md5,"chunks":[{"name":"fixture","md5":md5,"offset":0,"size":1,"dsize":bytes.len()}]}));
    }
    let manifest = active.join("manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(
            &json!({"chunk_url_prefix":"https://fixture.invalid/chunks", "assets":assets}),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(active.join("source.json"), serde_json::to_vec(&json!({"manifests":[manifest],"export_prefixes":mirage_ffi::sophon::install::ASSET_PREFIXES,"seed_roots":[seeds],
        "cache_root":root.join("cache"),"disk_budget_bytes":4194304,"disk_free_floor_bytes":0,"memory_budget_bytes":0})).unwrap()).unwrap();
    fs::write(root.join("game.json"), serde_json::to_vec(&json!({"root":root,"seed_root":seeds,"language":"en-us","retention":"disposable","total_budget_bytes":1073741824,
        "cache_budget_bytes":4194304,"startup_budget_bytes":0,"disk_free_floor_bytes":0,"launcher":null})).unwrap()).unwrap();
    fs::write(root.join("session.json"), serde_json::to_vec(&json!({"format_version":1,"phase":"ready","active_generation":active,"previous_generation":null,
        "prepared":[],"error":null,"host_pid":null,"game_pid":null,"reclaimed_bytes":0})).unwrap()).unwrap();
    // Deliberately exclude WinFsp from inherited PATH: production must resolve it.
    let search = std::env::join_paths(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).filter(|path| {
            !path
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("winfsp")
        }),
    )
    .unwrap();
    let started = Instant::now();
    let mut command = Command::new(bundle.join("mirage.exe"));
    command
        .args(["game", "play", "--root"])
        .arg(&root)
        .env("PATH", search)
        .stdout(Stdio::from(
            fs::File::create(temporary.path().join("worker.stdout.log")).unwrap(),
        ))
        .stderr(Stdio::from(
            fs::File::create(temporary.path().join("worker.stderr.log")).unwrap(),
        ));
    if fail_monitor {
        command.env("MIRAGE_TEST_GAME_MONITOR_FAILURE", "1");
    }
    let mut worker = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut playing_seen = false;
    while worker.try_wait().unwrap().is_none() {
        if let Ok(bytes) = fs::read(root.join("session.json"))
            && let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            playing_seen |= state["phase"] == "playing" || state["phase"] == "monitoring_failed";
            if state["phase"] == "playing" {
                assert!(
                    mirage_cli::commands::games::cleanup_game(&root).is_err(),
                    "cleanup cannot race the worker"
                );
                if crash_worker {
                    worker.kill().unwrap();
                    break;
                }
            }
        }
        if Instant::now() >= deadline {
            let _ = worker.kill();
            panic!("synthetic session deadline exceeded");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let result = worker.wait_with_output().unwrap();
    assert_eq!(
        result.status.success(),
        !fail_monitor && !crash_worker,
        "{}",
        fs::read_to_string(temporary.path().join("worker.stderr.log")).unwrap()
    );
    if crash_worker {
        let deadline = Instant::now() + Duration::from_secs(15);
        let child_pid = loop {
            if let Ok(text) = fs::read_to_string(native.join("fixture-child-pid.txt")) {
                break text.parse::<u32>().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "detached child identity not recorded"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        let child = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
        assert!(!child.is_null(), "detached child must still be active");
        assert!(
            mirage_cli::commands::games::cleanup_game(&root).is_err(),
            "recovery must refuse an active detached child"
        );
        fs::write(
            native.join("fixture-release-child.txt"),
            b"cleanup refusal verified",
        )
        .unwrap();
        assert_eq!(unsafe { WaitForSingleObject(child, 15000) }, WAIT_OBJECT_0);
        unsafe { CloseHandle(child) };
        // EOF must preserve an orphan source even if only the launcher was
        // observed. Recovery, not that incomplete handle list, proves safety.
        let owner = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("cache/owner.lock"))
            .unwrap();
        assert!(
            owner.try_lock().is_err(),
            "orphan source must remain owned until exclusive recovery"
        );
        drop(owner);
        let recovery = Command::new(bundle.join("mirage.exe"))
            .args(["game", "cleanup", "--root"])
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            recovery.status.success(),
            "{}",
            String::from_utf8_lossy(&recovery.stderr)
        );
    }
    assert!(
        !(fail_monitor || crash_worker) || native.join("fixture-read-ok.txt").is_file(),
        "controller failure must not unmount a still-active game's reads"
    );
    assert!(
        playing_seen,
        "detached actual game must be observed after launcher exit"
    );
    assert!(
        started.elapsed()
            >= Duration::from_millis(if fail_monitor || crash_worker {
                1500
            } else {
                10000
            }),
        "handoff drain must precede cleanup"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("session.json")).unwrap()).unwrap();
    assert_eq!(
        state["phase"],
        if fail_monitor {
            "launch_failed"
        } else {
            "ready"
        }
    );
    assert!(state["host_pid"].is_null());
    assert_eq!(fs::read(seeds.join(name)).unwrap(), bytes);
    let profile: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("cache/access-profile-v1.json")).unwrap())
            .unwrap();
    assert!(
        fail_monitor || !profile["entries"].as_array().unwrap().is_empty(),
        "actual game PID reads must populate next-session learning"
    );
}

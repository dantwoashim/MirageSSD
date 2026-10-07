#![allow(unsafe_code)]

//! Small-file cost probe through the raw FFI (no WinFsp, no antivirus on a
//! mounted path): times each engine call of the create / stat / read /
//! rename / delete cycle separately so the per-file cost can be attributed.
//! Ignored by default; run with
//! `cargo test --release -p mirage-ffi --test small_file_probe -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageFileInfo, MirageStatus,
    mirage_engine_create_managed, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_file_close, mirage_file_stat, mirage_lookup, mirage_namespace_create,
    mirage_namespace_delete, mirage_namespace_rename, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const FILES: usize = 500;
const DIRS: usize = 10;
const BYTES: usize = 4096;

fn build_index() -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), b"seed").expect("seed file");
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
    (objects, index)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn engine(index: &Path, state_root: &Path) -> *mut MirageEngineHandle {
    let index16 = utf16(&index.to_string_lossy());
    let root16 = utf16(&state_root.to_string_lossy());
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    let status = unsafe {
        mirage_engine_create_managed(
            index16.as_ptr(),
            index16.len(),
            root16.as_ptr(),
            root16.len(),
            std::ptr::null(),
            0,
            1 << 30,
            &mut engine,
        )
    };
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    engine
}

#[derive(Default)]
struct Timer {
    total: Duration,
    count: u32,
}

impl Timer {
    fn time<T>(&mut self, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = work();
        self.total += started.elapsed();
        self.count += 1;
        value
    }

    fn report(&self, label: &str) {
        let mean = if self.count == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1e6 / f64::from(self.count)
        };
        println!(
            "{label:<24} n={:<5} mean {mean:>10.1} us  total {:>8.1} ms",
            self.count,
            self.total.as_secs_f64() * 1e3
        );
    }
}

fn lookup(engine: *mut MirageEngineHandle, path: &str) -> *mut MirageFileHandle {
    let path16 = utf16(path);
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    let status = unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) };
    assert_eq!(status, MirageStatus::Ok, "lookup {path}");
    file
}

fn content(index: usize) -> Vec<u8> {
    (0..BYTES)
        .map(|offset| ((index * 31 + offset * 7) % 251) as u8)
        .collect()
}

#[test]
#[ignore = "timing probe; run explicitly with --release --ignored --nocapture"]
fn small_file_cost_probe() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = engine(&index, state_root.path());
    let paths: Vec<String> = (0..FILES)
        .map(|i| format!("\\d{:02}\\f{i:05}.bin", i % DIRS))
        .collect();

    let mut mkdir = Timer::default();
    for dir in 0..DIRS {
        let path = utf16(&format!("\\d{dir:02}"));
        let status =
            mkdir.time(|| unsafe { mirage_namespace_create(engine, path.as_ptr(), path.len(), 1) });
        assert_eq!(status, MirageStatus::Ok);
    }

    let (mut create, mut open, mut write, mut close) = Default::default();
    let create_phase = Instant::now();
    for (i, path) in paths.iter().enumerate() {
        let path16 = utf16(path);
        let status = Timer::time(&mut create, || unsafe {
            mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0)
        });
        assert_eq!(status, MirageStatus::Ok);
        let file = Timer::time(&mut open, || lookup(engine, path));
        let data = content(i);
        let mut transferred = 0usize;
        let status = Timer::time(&mut write, || unsafe {
            mirage_write(file, 0, data.as_ptr(), data.len(), &mut transferred)
        });
        assert_eq!((status, transferred), (MirageStatus::Ok, BYTES));
        let status = Timer::time(&mut close, || unsafe { mirage_file_close(file) });
        assert_eq!(status, MirageStatus::Ok);
    }
    let create_phase = create_phase.elapsed();

    let (mut stat_open, mut stat_call, mut stat_close) = Default::default();
    let stat_phase = Instant::now();
    for path in &paths {
        let file = Timer::time(&mut stat_open, || lookup(engine, path));
        let mut info = MirageFileInfo {
            stable_index: 0,
            size: 0,
            directory: 0,
            reserved: [0; 7],
            created_ns: 0,
            modified_ns: 0,
        };
        let status = Timer::time(&mut stat_call, || unsafe {
            mirage_file_stat(file, &mut info)
        });
        assert_eq!((status, info.size), (MirageStatus::Ok, BYTES as u64));
        assert_eq!(
            Timer::time(&mut stat_close, || unsafe { mirage_file_close(file) }),
            MirageStatus::Ok
        );
    }
    let stat_phase = stat_phase.elapsed();

    let (mut read_open, mut read_call, mut read_close) = Default::default();
    let read_phase = Instant::now();
    for (i, path) in paths.iter().enumerate() {
        let file = Timer::time(&mut read_open, || lookup(engine, path));
        let mut buffer = vec![0u8; BYTES];
        let mut transferred = 0usize;
        let status = Timer::time(&mut read_call, || unsafe {
            mirage_read(file, 0, buffer.as_mut_ptr(), BYTES, &mut transferred)
        });
        assert_eq!((status, transferred), (MirageStatus::Ok, BYTES));
        assert_eq!(buffer, content(i), "content of {path}");
        assert_eq!(
            Timer::time(&mut read_close, || unsafe { mirage_file_close(file) }),
            MirageStatus::Ok
        );
    }
    let read_phase = read_phase.elapsed();

    let mut rename = Timer::default();
    let mut current = paths.clone();
    for i in (0..FILES).step_by(10) {
        let from = utf16(&current[i]);
        let target = format!("{}.renamed", current[i]);
        let to = utf16(&target);
        let status = rename.time(|| unsafe {
            mirage_namespace_rename(engine, from.as_ptr(), from.len(), to.as_ptr(), to.len())
        });
        assert_eq!(status, MirageStatus::Ok);
        current[i] = target;
    }

    let mut delete = Timer::default();
    for path in &current {
        let path16 = utf16(path);
        let status = delete
            .time(|| unsafe { mirage_namespace_delete(engine, path16.as_ptr(), path16.len()) });
        assert_eq!(status, MirageStatus::Ok, "delete {path}");
    }

    println!("=== small-file FFI probe: {FILES} x {BYTES} B in {DIRS} dirs ===");
    mkdir.report("mkdir (namespace_create)");
    create.report("create (namespace_create)");
    open.report("create: lookup");
    write.report("create: write 4 KiB");
    close.report("create: close");
    println!(
        "create phase per file: {:.1} us",
        create_phase.as_secs_f64() * 1e6 / FILES as f64
    );
    stat_open.report("stat: lookup");
    stat_call.report("stat: file_stat");
    stat_close.report("stat: close");
    println!(
        "stat phase per file: {:.1} us",
        stat_phase.as_secs_f64() * 1e6 / FILES as f64
    );
    read_open.report("read: lookup");
    read_call.report("read: read 4 KiB");
    read_close.report("read: close");
    println!(
        "read phase per file: {:.1} us",
        read_phase.as_secs_f64() * 1e6 / FILES as f64
    );
    rename.report("rename");
    delete.report("delete");
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use mirage_manifest::FileClass;
use mirage_pack::import::import_local_with_cancel;
use mirage_pack::{ImportPlan, PackReader, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn plan(source: &std::path::Path, output: &std::path::Path) -> ImportPlan {
    ImportPlan {
        repository_id: RepositoryId::from_bytes([1; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.to_path_buf(),
        files: vec![
            PlannedFile {
                relative_path: "bin/game.exe".into(),
                class: FileClass::NativeExecutable,
            },
            PlannedFile {
                relative_path: "assets/first.pak".into(),
                class: FileClass::VirtualContainer,
            },
            PlannedFile {
                relative_path: "assets/second.pak".into(),
                class: FileClass::VirtualContainer,
            },
        ],
        page_size: 64 * 1024,
        pack_target: 150 * 1024,
        output_staging_directory: output.to_path_buf(),
        encryption: None,
    }
}

fn source_tree() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    let source = tempfile::tempdir().expect("source");
    std::fs::create_dir_all(source.path().join("bin")).expect("bin");
    std::fs::create_dir_all(source.path().join("assets")).expect("assets");
    std::fs::write(source.path().join("bin/game.exe"), b"native").expect("native");
    let shared = vec![0x5a; 64 * 1024];
    let mut first = shared.clone();
    first.extend(vec![0x11; 64 * 1024]);
    let mut second = shared;
    second.extend(vec![0x22; 31_337]);
    std::fs::write(source.path().join("assets/first.pak"), &first).expect("first");
    std::fs::write(source.path().join("assets/second.pak"), &second).expect("second");
    (source, first, second)
}

#[test]
fn import_deduplicates_and_reconstructs_every_virtual_file() {
    let (source, first, second) = source_tree();
    let output_parent = tempfile::tempdir().expect("output parent");
    let output = output_parent.path().join("import");
    let imported = import_local(&plan(source.path(), &output)).expect("import");
    assert!(imported.report.reused_pages >= 1);
    assert!(imported.report.unique_page_bytes < imported.report.virtual_bytes);
    assert!(
        !std::fs::read(&imported.manifest_path)
            .expect("manifest bytes")
            .is_empty()
    );

    let expected = HashMap::from([("first.pak", first), ("second.pak", second)]);
    for file in imported
        .manifest
        .files
        .iter()
        .filter(|file| file.class.is_virtual())
    {
        let mut reconstructed = Vec::new();
        let extent = &imported.manifest.extents[file.extent_start as usize];
        for page in &imported.manifest.pages
            [extent.page_start as usize..(extent.page_start + extent.page_count) as usize]
        {
            let location = &imported.manifest.remote_locations[page.remote_location as usize];
            let pack_path = output.join(location.object.provider_object_id.as_str());
            let mut pack_file = std::fs::File::open(pack_path).expect("pack");
            let mut frame = vec![0_u8; location.encoded_length.as_u64() as usize];
            pack_file
                .seek(SeekFrom::Start(location.offset))
                .expect("seek");
            pack_file.read_exact(&mut frame).expect("frame");
            reconstructed.extend_from_slice(
                &mirage_pack::decode_plain_frame(&frame)
                    .expect("decode")
                    .page
                    .bytes,
            );
        }
        assert_eq!(
            &reconstructed,
            expected.get(file.name.as_str()).expect("expected")
        );
    }
}

#[test]
fn cancellation_leaves_only_verified_packs_and_resume_reuses_them() {
    let (source, _, _) = source_tree();
    let output_parent = tempfile::tempdir().expect("output parent");
    let output = output_parent.path().join("import");
    let mut checks = 0;
    let cancelled = import_local_with_cancel(&plan(source.path(), &output), || {
        checks += 1;
        checks > 2
    });
    assert!(cancelled.is_err());
    assert!(
        !std::fs::read_dir(&output)
            .expect("output")
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("tmp"))
    );
    for entry in std::fs::read_dir(&output).expect("output") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|value| value.to_str()) == Some("bin") {
            PackReader::open_verified(&path).expect("surviving pack must verify");
        }
    }
    let resumed = import_local(&plan(source.path(), &output)).expect("resume");
    assert!(resumed.report.reused_pages > 0);
}

#[test]
fn output_inside_source_is_refused_without_deleting_source() {
    let (source, first, _) = source_tree();
    let output = source.path().join("generated");
    let result = import_local(&plan(source.path(), &output));
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(source.path().join("assets/first.pak")).unwrap(),
        first
    );
}

#![allow(dead_code)]

use mirage_manifest::{
    DecodeLimits, DirectoryRecord, FileClass, FileRecord, RepositoryManifest,
    decode_manifest_bounded, validate_manifest,
};
use mirage_types::{ByteCount, StableFileId};

const COMPLEX_MANIFEST: &[u8] =
    include_bytes!("../../../mirage-manifest/tests/fixtures/manifest-v2-complex.cbor");

pub fn complex_manifest() -> RepositoryManifest {
    decode_manifest_bounded(COMPLEX_MANIFEST, DecodeLimits::default())
        .expect("decode shared complex manifest fixture")
}

pub fn namespace_manifest() -> RepositoryManifest {
    let mut manifest = complex_manifest();
    manifest.directories.extend([
        DirectoryRecord {
            parent: Some(0),
            name: "Empty".to_string(),
        },
        DirectoryRecord {
            parent: Some(1),
            name: "Deep".to_string(),
        },
        DirectoryRecord {
            parent: Some(3),
            name: "LongDirectoryName".repeat(8),
        },
    ]);
    let next_extent = u32::try_from(manifest.extents.len()).expect("extent count fits u32");
    manifest.files.extend([
        FileRecord {
            parent_directory: 0,
            name: "Alpha.txt".to_string(),
            logical_size: ByteCount::from_u64(10),
            stable_id: StableFileId::from_u64(101),
            class: FileClass::NativeMutable,
            extent_start: next_extent,
            extent_count: 0,
        },
        FileRecord {
            parent_directory: 0,
            name: "zulu.txt".to_string(),
            logical_size: ByteCount::from_u64(20),
            stable_id: StableFileId::from_u64(102),
            class: FileClass::NativeMutable,
            extent_start: next_extent,
            extent_count: 0,
        },
        FileRecord {
            parent_directory: 3,
            name: "MiXeD.bin".to_string(),
            logical_size: ByteCount::from_u64(30),
            stable_id: StableFileId::from_u64(103),
            class: FileClass::NativeMutable,
            extent_start: next_extent,
            extent_count: 0,
        },
    ]);
    validate_manifest(&manifest).expect("valid namespace manifest");
    manifest
}

pub fn semantically_reordered(mut manifest: RepositoryManifest) -> RepositoryManifest {
    manifest.files.reverse();
    let old_directories = manifest.directories.clone();
    let order: Vec<usize> = (0..old_directories.len()).rev().collect();
    let mut old_to_new = vec![0_u32; order.len()];
    for (new, old) in order.iter().copied().enumerate() {
        old_to_new[old] = u32::try_from(new).expect("directory index fits u32");
    }
    manifest.directories = order
        .iter()
        .map(|old| {
            let mut directory = old_directories[*old].clone();
            directory.parent = directory.parent.map(|parent| old_to_new[parent as usize]);
            directory
        })
        .collect();
    for file in &mut manifest.files {
        file.parent_directory = old_to_new[file.parent_directory as usize];
    }
    validate_manifest(&manifest).expect("valid reordered manifest");
    manifest
}

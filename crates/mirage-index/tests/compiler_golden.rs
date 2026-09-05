mod support;

use std::fs;

use mirage_index::{Header, MountIndex, compile_to_bytes, compile_to_path};

use support::{complex_manifest, semantically_reordered};

const GOLDEN: &[u8] = include_bytes!("fixtures/index-v2/complex.midx");
const GOLDEN_HASH: &str = "2a36291616c96502dcb253ea42b73274ac19c9b526a0442b06709412690bd022";

#[test]
fn compiler_matches_exact_golden_bytes_and_hash() {
    let bytes = compile_to_bytes(&complex_manifest()).expect("compile index");
    assert_eq!(bytes, GOLDEN);
    let header = Header::parse(&bytes).expect("header");
    assert_eq!(hex(&header.index_hash), GOLDEN_HASH);
}

#[test]
fn manifest_insertion_order_does_not_change_index_bytes() {
    let manifest = complex_manifest();
    let reordered = semantically_reordered(manifest.clone());
    assert_eq!(
        compile_to_bytes(&manifest).expect("compile original"),
        compile_to_bytes(&reordered).expect("compile reordered")
    );
}

#[test]
fn publication_reopens_verifies_and_never_replaces_an_existing_index() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let destination = temporary.path().join("mount.midx");
    compile_to_path(&complex_manifest(), &destination).expect("publish index");
    let mounted = MountIndex::open(&destination).expect("map published index");
    assert_eq!(mounted.file_count(), 2);
    let before = fs::read(&destination).expect("published bytes");
    assert!(compile_to_path(&complex_manifest(), &destination).is_err());
    assert_eq!(fs::read(&destination).expect("unchanged bytes"), before);
}

#[test]
fn failed_temporary_write_cannot_leave_a_final_index() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let non_directory = temporary.path().join("not-a-directory");
    fs::write(&non_directory, b"occupied").expect("sentinel");
    let destination = non_directory.join("mount.midx");
    assert!(compile_to_path(&complex_manifest(), &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(fs::read(non_directory).expect("sentinel"), b"occupied");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

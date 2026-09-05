use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use mirage_manifest::{commit_hash, encode_commit, encode_manifest, manifest_hash, sign_commit};
use mirage_types::RepositoryId;

#[path = "../../tests/support/mod.rs"]
mod support;

fn main() -> Result<(), Box<dyn Error>> {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::create_dir_all(fixture_root.join("commit-chain"))?;

    let manifests = [
        ("manifest-v2-minimal.cbor", support::minimal_manifest()),
        ("manifest-v2-complex.cbor", support::complex_manifest()),
    ];
    let mut manifest_readme = String::from(
        "# Canonical manifest v2 fixtures\n\nGenerated only by `generate-manifest-fixtures`. Hashes are BLAKE3 over exact canonical CBOR bytes.\n\n",
    );
    for (name, manifest) in manifests {
        let bytes = encode_manifest(&manifest)?;
        fs::write(fixture_root.join(name), &bytes)?;
        writeln!(
            manifest_readme,
            "- `{name}`: {} bytes, `{}`",
            bytes.len(),
            manifest_hash(&manifest)?
        )?;
    }
    fs::write(fixture_root.join("README.md"), manifest_readme)?;

    let signer = support::signer();
    let repository_id = RepositoryId::from_bytes([0x61; 16]);
    let mut parent = None;
    let mut commit_readme = String::from(
        "# Canonical commit v1 chain fixtures\n\nTen signed commits forming one exact linear chain. Hashes are BLAKE3 over canonical signed commit CBOR. The keyed signer is test-only.\n\n",
    );
    for sequence in 0_u64..10 {
        let commit = sign_commit(
            support::commit_body(repository_id, sequence, parent, u8::try_from(sequence + 1)?),
            &signer,
        )?;
        let bytes = encode_commit(&commit)?;
        let hash = commit_hash(&commit)?;
        let name = format!("commit-{sequence:04}.cbor");
        fs::write(fixture_root.join("commit-chain").join(&name), &bytes)?;
        writeln!(commit_readme, "- `{name}`: {} bytes, `{hash}`", bytes.len())?;
        parent = Some(hash);
    }
    fs::write(fixture_root.join("commit-chain/README.md"), commit_readme)?;
    Ok(())
}

use std::error::Error;
use std::fs;
use std::path::Path;

use mirage_index::{Header, compile_to_bytes};
use mirage_manifest::{DecodeLimits, decode_manifest_bounded};

const MANIFEST: &[u8] =
    include_bytes!("../../../mirage-manifest/tests/fixtures/manifest-v2-complex.cbor");

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = decode_manifest_bounded(MANIFEST, DecodeLimits::default())?;
    let bytes = compile_to_bytes(&manifest)?;
    let header = Header::parse(&bytes)?;
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index-v2");
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("complex.midx"), &bytes)?;
    fs::write(
        directory.join("README.md"),
        format!(
            "# MIRIDX02 fixtures\n\n`complex.midx` is {} bytes with canonical index hash `{}`. Regenerate only with `generate-index-fixture`.\n",
            bytes.len(),
            hex(&header.index_hash)
        ),
    )?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

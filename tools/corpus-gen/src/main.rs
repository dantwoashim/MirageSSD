use std::io::Write;
use std::path::{Path, PathBuf};

use mirage_corpus_gen::{CorpusProfile, describe, fill_at, plan};

const DEFAULT_MATERIALIZE_LIMIT: u64 = 1024 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().is_some_and(|value| value == "--frames") {
        let output = arguments
            .get(1)
            .map_or_else(|| PathBuf::from("tests/fixtures/pack-v1"), PathBuf::from);
        return generate_frames(&output);
    }
    let profile: CorpusProfile = value(&arguments, "--profile")
        .ok_or("missing --profile")?
        .parse()?;
    let seed = value(&arguments, "--seed").unwrap_or("1").parse::<u64>()?;
    let output = PathBuf::from(value(&arguments, "--output").ok_or("missing --output")?);
    let materialize = arguments.iter().any(|value| value == "--materialize");
    let limit = value(&arguments, "--max-materialized-bytes")
        .map_or(Ok(DEFAULT_MATERIALIZE_LIMIT), str::parse::<u64>)?;
    let mut plan = plan(profile, seed);
    if let Some(requested) = value(&arguments, "--logical-bytes") {
        let mut remaining = requested.parse::<u64>()?;
        for file in &mut plan.files {
            file.logical_length = file.logical_length.min(remaining);
            remaining -= file.logical_length;
        }
        plan.files.retain(|file| file.logical_length != 0);
        plan.total_logical_bytes = plan.files.iter().map(|file| file.logical_length).sum();
    }
    std::fs::create_dir_all(&output)?;
    let descriptor = describe(&plan);
    std::fs::write(
        output.join("corpus.json"),
        serde_json::to_vec_pretty(&descriptor)?,
    )?;
    if materialize {
        if plan.total_logical_bytes > limit {
            return Err(format!(
                "materialization requires {} bytes, above the explicit limit {}",
                plan.total_logical_bytes, limit
            )
            .into());
        }
        materialize_plan(&plan, &output)?;
    }
    Ok(())
}

fn value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn materialize_plan(
    plan: &mirage_corpus_gen::CorpusPlan,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    for corpus_file in &plan.files {
        let path = output.join(&corpus_file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        let mut offset = 0_u64;
        while offset < corpus_file.logical_length {
            let length =
                usize::try_from((corpus_file.logical_length - offset).min(buffer.len() as u64))?;
            fill_at(
                corpus_file.seed,
                corpus_file.pattern,
                offset,
                &mut buffer[..length],
            );
            file.write_all(&buffer[..length])?;
            offset += length as u64;
        }
        file.sync_all()?;
    }
    Ok(())
}

fn generate_frames(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use bytes::Bytes;
    use mirage_pack::{PlainPage, encode_plain_frame};
    std::fs::create_dir_all(output)?;
    let tail = encode_plain_frame(&PlainPage::from_bytes(Bytes::from_static(
        b"Mirage pack v1 tail",
    )))?;
    let full = encode_plain_frame(&PlainPage::from_bytes(Bytes::from(vec![0x5a; 64 * 1024])))?;
    std::fs::write(output.join("frame-tail.bin"), &tail)?;
    std::fs::write(output.join("frame-full.bin"), &full)?;
    let mut corrupt = tail.clone();
    corrupt[8] ^= 0x80;
    std::fs::write(output.join("frame-corrupt-header.bin"), corrupt)?;
    std::fs::write(output.join("frame-truncated.bin"), &tail[..tail.len() - 1])?;
    Ok(())
}

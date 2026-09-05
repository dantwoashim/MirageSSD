use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use mirage_predictor::{TraceBlockEncoder, TraceEvent, TraceHeader};
use mirage_types::{GenerationId, RepositoryId, StableFileId};

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, value_enum)]
    pattern: Pattern,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 10_000)]
    events: u32,
    #[arg(long, default_value_t = 0x37)]
    seed: u64,
    #[arg(long, default_value_t = 1_048_576)]
    page_size: u32,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Pattern {
    Sequential,
    Random,
    Phase,
    Branch,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.events == 0 || args.page_size == 0 {
        return Err("events and page-size must be non-zero".into());
    }
    let header = TraceHeader {
        schema_version: 1,
        repository_id: RepositoryId::from_bytes([0x37; 16]),
        manifest_generation: GenerationId::ZERO,
        page_size: args.page_size,
        machine_profile: "synthetic-trace-gen".into(),
        dropped_event_count: 0,
    };
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)?;
    let mut encoder = TraceBlockEncoder::new(file, &header)?;
    let mut state = args.seed;
    for block_start in (0..args.events).step_by(4096) {
        let block_end = args.events.min(block_start + 4096);
        let mut block = Vec::with_capacity((block_end - block_start) as usize);
        for index in block_start..block_end {
            state = splitmix64(state);
            let page = match args.pattern {
                Pattern::Sequential => u64::from(index),
                Pattern::Random => state % 65_536,
                Pattern::Phase => u64::from(index / 1000) * 4096 + u64::from(index % 128),
                Pattern::Branch => {
                    if state & 7 == 0 {
                        32_768 + state % 1024
                    } else {
                        u64::from(index % 2048)
                    }
                }
            };
            block.push(TraceEvent {
                timestamp_ns: u64::from(index) * 100_000,
                stable_file_id: StableFileId::from_u64(1),
                offset: page * u64::from(args.page_size),
                length: args.page_size,
                flags: 0,
            });
        }
        encoder.write_block(&block)?;
    }
    let _ = encoder.finish();
    Ok(())
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

"""Bounded, read-only local corpus compression experiment; stdlib zlib only.

Files retain their own boundaries. This does not extrapolate zlib results to
zstd/LZ4 or to encrypted game data. No source file or mount is modified.
"""
import argparse
import hashlib
import json
from pathlib import Path
import random
import time
import zlib


def sample(paths, cap=16 << 20):
    blobs = []
    total = 0
    for path in sorted(paths):
        if total == cap:
            break
        with path.open("rb") as f:
            data = f.read(cap-total)
        if data:
            blobs.append(data)
            total += len(data)
    return blobs


def measure(name, blobs, block):
    raw = sum(map(len, blobs))
    chunks = [b[i:i+block] for b in blobs for i in range(0, len(b), block)]
    start = time.perf_counter()
    compressed = [zlib.compress(b, 1) for b in chunks]
    encode_seconds = time.perf_counter()-start
    start = time.perf_counter()
    assert all(zlib.decompress(c) == b for c, b in zip(compressed, chunks))
    decode_and_compare_seconds = time.perf_counter()-start
    unique = {}
    for b in chunks:
        unique[hashlib.sha256(b).digest()] = len(b)
    # Codec flag/length/index costs are not included. Store raw if compression
    # loses; selected bytes include zlib stream overhead.
    selected = sum(min(len(c), len(b)) for c,b in zip(compressed,chunks))
    return {"corpus": name, "files_or_blobs": len(blobs), "raw_bytes": raw,
            "block_bytes": block, "compressed_or_raw_fraction": selected/raw,
            "encode_MiB_s": raw/2**20/encode_seconds,
            "decode_compare_MiB_s": raw/2**20/decode_and_compare_seconds,
            "duplicate_fraction_exact_fixed_chunks": 1-sum(unique.values())/raw,
            "corpus_sha256": hashlib.sha256(b"".join(blobs)).hexdigest()}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", type=Path, default=Path("target/perf/research-20261012"))
    a = p.parse_args()
    root = Path(__file__).resolve().parents[2]
    source = [p for directory in ["crates", "docs", "native", "apps/mirage-ui/src"]
              for p in (root/directory).rglob("*")
              if p.is_file() and p.suffix in {".rs", ".md", ".cpp", ".h", ".tsx", ".ts", ".css"}]
    rng = random.Random(20261012)
    corpora = {"repository_source": sample(source),
               "installed_mirage_binaries": sample(Path("C:/Program Files/MirageSSD").glob("*.exe")),
               "synthetic_incompressible_control": [rng.randbytes(8 << 20)]}
    rows = [measure(name, blobs, block) for name,blobs in corpora.items() if blobs
            for block in [64 << 10, 256 << 10, 4 << 20]]
    result = {"kind": "measured_local_zlib1", "rows": rows,
              "caveats": "Deterministic sorted samples, not representative of the user drive. Timing includes Python overhead; decoding includes byte comparison. No codec/index/storage-allocation metadata is counted."}
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out/"compression.json").write_text(json.dumps(result,indent=2),encoding="utf-8")
    for r in rows:
        print(r["corpus"], r["block_bytes"]//1024, "KiB", round(r["compressed_or_raw_fraction"]*100,2), "% retained",
              round(r["decode_compare_MiB_s"]), "MiB/s decode+compare")


if __name__ == "__main__":
    main()

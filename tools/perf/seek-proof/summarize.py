"""Validate and archive the seek-proof probe outputs (Python standard library)."""

import argparse
import hashlib
import json
import statistics
import subprocess
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]


def load(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=ROOT / "target/perf/research-20261012")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    cpu_path = args.input / "seek-proof.json"
    transport_path = args.input / "seek-proof-transport.json"
    cpu, transport = load(cpu_path), load(transport_path)
    proof = cpu["existing_root_proof"]
    assert cpu["test_key_only"] and cpu["legacy_ciphertext_unchanged"]
    assert not proof["new_trust_anchor_required"]
    assert proof["payload_reupload_bytes"] == 0
    assert proof["positive_byte_exact_cases"] >= 10742
    assert proof["negative_rejection_cases"] >= 1588
    assert proof["trees_compared_to_standard_blake3_hash"] >= 206
    assert proof["benchmark_reads_per_variant"] == 800
    assert len(proof["length_cases"]) == 89
    assert len({row["checksum"] for row in proof["samples"]}) == 1
    variants = {"whole_frame", "root_proof_4KiB", "root_proof_64KiB", "root_proof_256KiB"}
    assert Counter(row["variant"] for row in proof["samples"]) == Counter(dict.fromkeys(variants, 8))
    assert len({(row["round"], row["variant"]) for row in proof["samples"]}) == 32
    cpu_summary = {}
    for variant in sorted(variants):
        rows = [r for r in proof["samples"] if r["variant"] == variant]
        assert all(r["p50_ns"] <= r["p99_ns"] for r in rows)
        assert len({r["response_content_bytes_per_read"] for r in rows}) == 1
        cpu_summary[variant] = {
            "median_of_round_p50_us": statistics.median(r["p50_ns"] / 1000 for r in rows),
            "median_of_round_p99_us": statistics.median(r["p99_ns"] / 1000 for r in rows),
            "response_content_bytes": rows[0]["response_content_bytes_per_read"],
        }
    expected_requests_and_bodies = {
        "whole_frame_one_request": (1, 4194372),
        "proof_cached_catalog": (1, 65536),
        "proof_serial_catalog": (2, 67636),
        "proof_parallel_catalog": (2, 67636),
    }
    assert transport["catalog_bytes"] == 2100
    assert transport["body_rate_limit_MiB_s"] == 20
    assert len(transport["rows"]) == 56
    assert len({(r["server_delay_ms"], r["round"], r["variant"]) for r in transport["rows"]}) == 56
    transport_summary = []
    for delay in (0, 40, 200, 2000):
        for variant, (requests, body) in expected_requests_and_bodies.items():
            rows = [r for r in transport["rows"] if r["variant"] == variant and r["server_delay_ms"] == delay]
            assert len(rows) == (2 if delay == 2000 else 4)
            assert all(r["http_requests"] == requests and r["response_body_bytes"] == body for r in rows)
            transport_summary.append({
                "server_delay_ms": delay, "variant": variant, "rounds": len(rows),
                "median_elapsed_ms": statistics.median(r["elapsed_ms"] for r in rows),
                "http_requests": requests, "response_body_bytes": body,
            })
    sources = [
        ROOT / "crates/mirage-crypto/src/aead.rs",
        ROOT / "crates/mirage-pack/src/encrypted_frame.rs",
        ROOT / "crates/mirage-types/Cargo.toml",
        *sorted((ROOT / "crates/mirage-types/src").rglob("*.rs")),
        ROOT / "tools/perf/seek-proof/Cargo.toml",
        ROOT / "tools/perf/seek-proof/Cargo.lock",
        *sorted((ROOT / "tools/perf/seek-proof/src").rglob("*.rs")),
        Path(__file__).resolve(),
    ]
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    evidence = {
        "research_date": "2026-10-12",
        "archive_generated_utc": datetime.now(timezone.utc).isoformat(),
        "repository_base": revision,
        "machine_recorded_for_these_runs": {
            "cpu": "Intel Core i5-1135G7", "physical_cores": 4, "logical_processors": 8,
            "visible_ram_KiB": 12216856, "os": "Windows 11 Home 10.0.26300",
            "rustc": "1.94.0 (4a4ef493e 2026-03-02)",
        },
        "provenance_scope": "Source hashes captured at archival time. HTTP measurement preceded the empty-input check and percentile-label correction; its nonempty fixture and HTTP harness were unaffected. All results are isolated research probes, not production or live Drive measurements.",
        "source_sha256": {p.relative_to(ROOT).as_posix(): digest(p) for p in sources},
        "raw_output_sha256": {p.name: digest(p) for p in (cpu_path, transport_path)},
        "summary": {"cpu": cpu_summary, "http": transport_summary},
        "cpu_and_correctness": cpu,
        "controlled_http": transport,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print(f"Validated 32 CPU rounds and 56 HTTP observations; archived {args.output}")
    print(json.dumps(cpu_summary, indent=2))


if __name__ == "__main__":
    main()

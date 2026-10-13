"""Reproducible cache research, not a mounted-product benchmark.

Uses synthetic fixed-size-frame traces with equal payload capacity. Includes
adversarial and low-locality traces; no timings are inferred from miss counts.
Writes only to the requested output directory. Standard library only.
"""
import argparse
from collections import OrderedDict, deque
import hashlib
import json
from pathlib import Path
import random


def replay(trace, capacity, policy):
    cache = OrderedDict()
    hand = 0
    misses = 0
    peak = 0
    future = {}
    history = OrderedDict()
    if policy == "oracle":
        for i, key in enumerate(trace):
            future.setdefault(key, deque()).append(i)
    for i, key in enumerate(trace):
        if policy == "periodic":
            old = history.pop(key, None)
            if old is None:
                history[key] = (i, 0, 0, 0)
            else:
                last, period, deviation, count = old
                delta = i - last
                history[key] = (i, delta if not count else .75 * period + .25 * delta,
                                0 if not count else .75 * deviation + .25 * abs(delta-period), count+1)
            while len(history) > 4 * capacity:
                history.popitem(last=False)
        if policy == "oracle":
            future[key].popleft()
        if key in cache:
            if policy in ("lru", "mru"):
                cache.move_to_end(key)
            elif policy == "sieve":
                cache[key] = True
            continue
        misses += 1
        if len(cache) == capacity:
            if policy == "clear_all":
                cache.clear()
            elif policy == "lru":
                cache.popitem(last=False)
            elif policy == "mru":
                cache.popitem(last=True)
            elif policy == "sieve":
                keys = list(cache)
                while cache[keys[hand]]:
                    cache[keys[hand]] = False
                    hand = (hand + 1) % len(keys)
                del cache[keys[hand]]
                # The hand continues toward newer insertions, wraps at end.
                # After deletion the successor takes this index.
                if hand == len(cache):
                    hand = 0
            elif policy == "oracle":
                victim = max(cache, key=lambda k: future[k][0] if future[k] else float("inf"))
                del cache[victim]
            elif policy == "periodic":
                # Research candidate: predict only after two observed intervals
                # with low dispersion. Unknown/stale estimates evict first.
                # No future trace access; history is bounded to 4x capacity.
                def predicted_next(k):
                    last, period, deviation, count = history.get(k, (0, 0, 0, 0))
                    if count < 2 or deviation > .25 * period or last + period <= i:
                        return float("inf")
                    return last + period
                victim = max(cache, key=predicted_next)
                del cache[victim]
        cache[key] = False
        peak = max(peak, len(cache))
        assert len(cache) <= capacity
    return {"requests": len(trace), "misses": misses,
            "miss_ratio": misses / len(trace), "peak_frames": peak}


def traces(seed):
    rng = random.Random(seed)
    hot_scan = []
    for epoch in range(60):
        hot_scan.extend(rng.randrange(24) for _ in range(240))
        hot_scan.extend(10000 + epoch * 64 + j for j in range(64))
    interleaved = []
    for i in range(15000):
        interleaved.append(rng.randrange(8) if i % 5 else 10000 + i)
    shifting = []
    for phase in range(8):
        shifting.extend(phase * 64 + rng.randrange(24) for _ in range(1500))
    return {
        "hot_set_with_scans": hot_scan,
        "hot_set_interleaved_one_hit": interleaved,
        "working_set_33_cycle": list(range(33)) * 400,
        "single_scan": list(range(16000)),
        "uniform_random_256": [rng.randrange(256) for _ in range(16000)],
        "phase_changes": shifting,
        "repeated_bursts": [i for i in range(4000) for _ in range(4)],
    }


def geometry():
    # Exact count of bytes necessary to retain one aligned 4 KiB read per
    # file, assuming distinct independent verification units and no prefetch.
    return [{"frame_KiB": size // 1024,
             "aligned_4K_read_amplification": size / 4096,
             "4096_distinct_file_headers_MiB": 4096 * size / 2**20,
             "wire_overhead_percent_68B": 100 * 68 / size,
             "hash_table_MiB_per_TiB_32B": (2**40 / size) * 32 / 2**20}
            for size in [64 << 10, 256 << 10, 1 << 20, 4 << 20]]


def sector_retention():
    # Existing-format compatibility: after verifying the whole legacy frame,
    # retaining just touched 64 KiB sectors reduces *retention*, not fetches.
    return [{"distinct_touched_sectors_per_4M_frame": sectors,
             "retained_KiB": sectors * 64,
             "retention_reduction_percent": 100 * (1 - sectors / 64),
             "legacy_first_fetch_MiB": 4}
            for sectors in [1, 2, 4, 16, 64]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path("target/perf/research-20261012"))
    args = parser.parse_args()
    # Sanity cases validate capacity and optimal-baseline interpretation.
    for p in ["clear_all", "lru", "mru", "sieve", "periodic", "oracle"]:
        assert replay([0, 1, 0, 1], 2, p)["misses"] == 2
        assert replay([0, 1, 0], 1, p)["misses"] == 3
    rows = []
    for seed in [17, 31, 73, 101, 223]:
        for name, trace in traces(seed).items():
            for capacity in [16, 32, 64]:
                results = {p: replay(trace, capacity, p)
                           for p in ["clear_all", "lru", "mru", "sieve", "periodic", "oracle"]}
                assert all(v["misses"] >= results["oracle"]["misses"]
                           for v in results.values())
                rows.append({"trace": name, "seed": seed,
                             "capacity_frames": capacity,
                             "trace_sha256": hashlib.sha256(json.dumps(trace).encode()).hexdigest(),
                             "policies": results})
    args.out.mkdir(parents=True, exist_ok=True)
    result = {"kind": "synthetic_fixed_frame_replay",
              "warning": "Not measured application speed or real workload cache savings. Payload capacity only; policy metadata is not modeled.",
              "frame_bytes_for_capacity_labels": 4 << 20,
              "rows": rows, "geometry": geometry(),
              "legacy_sector_retention": sector_retention(),
              "streaming_lower_bounds": [
                  {"source_latency_s": latency, "consumer_MiB_s": rate,
                   "minimum_lead_MiB": latency * rate}
                  for latency in [0.04, 0.2, 2.0] for rate in [5, 20, 32]],
              "source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    (args.out / "cache-model.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    for row in rows:
        if row["seed"] == 17 and row["capacity_frames"] == 32:
            print(row["trace"], {p: round(v["miss_ratio"] * 100, 2)
                                  for p, v in row["policies"].items()})
    print("Wrote", args.out / "cache-model.json", "(105 trace/capacity/seed comparisons)")


if __name__ == "__main__":
    main()

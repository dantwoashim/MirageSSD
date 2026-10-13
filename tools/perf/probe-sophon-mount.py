"""Read selected whole assets through a mount and check pinned manifest identity.

This deliberately measures application reads plus MD5, not disk-only throughput
or gameplay. Give the same assets to every candidate and alternate run order.
"""
import argparse
import hashlib
import json
import pathlib
import time

parser = argparse.ArgumentParser()
parser.add_argument("--mount", type=pathlib.Path, required=True)
parser.add_argument("--manifest", type=pathlib.Path, action="append", required=True)
parser.add_argument("--asset", action="append", required=True)
parser.add_argument("--report", type=pathlib.Path, required=True)
args = parser.parse_args()
assets = {}
for manifest in args.manifest:
    for asset in json.loads(manifest.read_text(encoding="utf-8"))["assets"]:
        assets[asset["name"]] = asset
results = []
for name in args.asset:
    asset = assets[name]
    start = time.perf_counter()
    digest = hashlib.md5()
    length = 0
    with (args.mount / name).open("rb") as file:
        while block := file.read(1024 * 1024):
            digest.update(block)
            length += len(block)
    results.append({"name": name, "bytes": length,
                    "seconds": time.perf_counter() - start,
                    "matched": length == asset["size"] and digest.hexdigest() == asset["md5"]})
args.report.parent.mkdir(parents=True, exist_ok=True)
args.report.write_text(json.dumps(results, indent=2), encoding="utf-8")
print(json.dumps({"files": len(results), "matched": sum(r["matched"] for r in results),
                  "bytes": sum(r["bytes"] for r in results)}))
raise SystemExit(0 if all(r["matched"] for r in results) else 1)

#!/usr/bin/env bash
# Builds the pinned, patched MirageSSD rclone provider for macOS.
# Mirrors scripts/build-rclone-miragessd.ps1: same upstream tag, commit, patch, and version suffix.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$script_dir/.." && pwd)"
source_dir="${1:-$repo/target/rclone-miragessd-source-v1.75.0}"
output_dir="${2:-$repo/target/rclone-miragessd}"

upstream='https://github.com/rclone/rclone.git'
tag='v1.75.0'
commit='9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048'
patch="$repo/third_party/rclone-miragessd/rclone-v1.75.0.patch"

[[ "$(uname -s)" == "Darwin" ]] || { echo 'This script builds the macOS provider; use build-rclone-miragessd.ps1 on Windows.' >&2; exit 1; }
command -v git >/dev/null || { echo 'Git is required to build the pinned MirageSSD rclone provider.' >&2; exit 1; }
command -v go >/dev/null || { echo 'Go is required to build the pinned MirageSSD rclone provider.' >&2; exit 1; }
# cgofuse links against the macFUSE SDK on macOS; there is no pure-Go fallback like on Windows.
[[ -d /Library/Filesystems/macfuse.fs ]] || { echo 'macFUSE must be installed (brew install --cask macfuse) to build the mount provider.' >&2; exit 1; }
[[ -f "$patch" ]] || { echo "Missing patch: $patch" >&2; exit 1; }

if [[ ! -d "$source_dir/.git" ]]; then
  git clone --filter=blob:none --branch "$tag" "$upstream" "$source_dir"
fi

actual_commit="$(git -C "$source_dir" rev-parse HEAD)"
if [[ "$actual_commit" != "$commit" ]]; then
  echo "Rclone source must be the pinned $tag commit $commit; found $actual_commit." >&2; exit 1
fi

if [[ -z "$(git -C "$source_dir" status --porcelain)" ]]; then
  git -C "$source_dir" apply --check "$patch" || { echo 'MirageSSD rclone patch does not apply to the pinned source.' >&2; exit 1; }
  git -C "$source_dir" apply "$patch"
else
  git -C "$source_dir" apply --reverse --check "$patch" 2>/dev/null \
    || { echo 'Rclone source contains changes other than the exact MirageSSD patch.' >&2; exit 1; }
fi

mkdir -p "$output_dir"
binary="$output_dir/rclone"
export CGO_ENABLED=1
(cd "$source_dir" && go test -tags cmount ./cmd/cmount)
(cd "$source_dir" && go build -trimpath \
  -ldflags '-s -w -X github.com/rclone/rclone/fs.VersionSuffix=miragessd2' \
  -tags cmount -o "$binary" .)

sha256="$(shasum -a 256 "$binary" | awk '{print $1}')"
version="$("$binary" version | head -n 1)"
printf '{"Built":true,"UpstreamTag":"%s","UpstreamCommit":"%s","Binary":"%s","Sha256":"%s","Version":"%s","Arch":"%s","BuildTag":"cmount"}\n' \
  "$tag" "$commit" "$binary" "$sha256" "$version" "$(uname -m)"

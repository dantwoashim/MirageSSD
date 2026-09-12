#!/usr/bin/env bash
# Packages MirageSSD.app for macOS: compiles the AppKit front end, bundles the pinned
# rclone provider and the Desktop OAuth application registration, signs, and emits
# a ZIP + DMG with checksums. The macOS counterpart of build-one-click-setup.ps1.
#
# Usage:
#   scripts/build-macos-app.sh --credentials ~/Downloads/desktop-oauth.json \
#       [--rclone target/rclone-miragessd/rclone] [--output ~/MirageSSD-Packages] \
#       [--version 0.1.0] [--sign "Developer ID Application: ..."] [--no-dmg]
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$script_dir/.." && pwd)"
credentials=''
rclone="$repo/target/rclone-miragessd/rclone"
rclone_source="$repo/target/rclone-miragessd-source-v1.75.0"
output="$HOME/MirageSSD-Packages"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)"
identity='-'
make_dmg=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --credentials) credentials="$2"; shift 2 ;;
    --rclone) rclone="$2"; shift 2 ;;
    --rclone-source) rclone_source="$2"; shift 2 ;;
    --output) output="$2"; shift 2 ;;
    --version) version="$2"; shift 2 ;;
    --sign) identity="$2"; shift 2 ;;
    --no-dmg) make_dmg=0; shift ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

fail() { echo "error: $*" >&2; exit 1; }
[[ "$(uname -s)" == "Darwin" ]] || fail 'MirageSSD.app must be built on macOS.'
command -v swiftc >/dev/null || fail 'Xcode Command Line Tools are required (xcode-select --install).'
[[ -n "$credentials" ]] || fail '--credentials <desktop-oauth.json> is required.'
[[ -f "$credentials" ]] || fail "Credentials file not found: $credentials"
[[ -x "$rclone" ]] || fail "Provider binary not found or not executable: $rclone (run scripts/build-rclone-miragessd.sh)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]] || fail "Version must be semver-like, got '$version'."
case "$credentials" in "$repo"/*) fail 'Keep the OAuth registration outside the source checkout.' ;; esac
case "$output" in "$repo"/*) fail 'Output must be outside the source checkout.' ;; esac

# Only Desktop-app (installed) registrations are public clients suitable for redistribution.
plutil -lint -s "$credentials" >/dev/null 2>&1 || python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$credentials" \
  || fail 'Credentials file is not valid JSON.'
python3 - "$credentials" <<'PY' || exit 1
import json, sys
data = json.load(open(sys.argv[1]))
installed = data.get("installed")
if not isinstance(installed, dict):
    sys.exit("error: expected a Google 'Desktop app' OAuth client JSON with an 'installed' object.")
if not str(installed.get("client_id", "")).endswith(".apps.googleusercontent.com") or not installed.get("client_secret"):
    sys.exit("error: OAuth registration is missing client_id/client_secret.")
PY

"$rclone" version | head -n 1 | grep -q 'miragessd2' || fail 'Provider is not the pinned v1.75.0-miragessd2 build.'
[[ -f "$rclone_source/COPYING" ]] || fail "Upstream rclone license not found at $rclone_source/COPYING (set --rclone-source)."

build="$(git -C "$repo" rev-list --count HEAD 2>/dev/null || echo 1)"
arch="$(uname -m)"
stage="$(mktemp -d "${TMPDIR:-/tmp}/miragessd-macos.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
app="$stage/MirageSSD.app"
contents="$app/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources/Notices"

echo "Compiling MirageSSD ($arch, $version+$build)..."
swiftc -O -target "$arch-apple-macos12.0" -module-name MirageSSD \
  -framework AppKit -framework Security \
  "$repo/apps/mirage-macos/main.swift" -o "$contents/MacOS/MirageSSD"

sed -e "s/MIRAGE_VERSION/$version/" -e "s/MIRAGE_BUILD/$build/" "$repo/apps/mirage-macos/Info.plist" > "$contents/Info.plist"
plutil -lint -s "$contents/Info.plist" || fail 'Generated Info.plist is invalid.'
printf 'APPL????' > "$contents/PkgInfo"

# The provider lives next to the main executable so codesign treats it as nested code,
# not a resource. main.swift resolves it relative to Bundle.main.executableURL.
cp "$rclone" "$contents/MacOS/rclone"
chmod 755 "$contents/MacOS/rclone"
# Installed desktop apps are public OAuth clients: ship the application registration only,
# never a user's token. The Swift front end refuses builds without this file.
cp "$credentials" "$contents/Resources/oauth-desktop.json"
chmod 644 "$contents/Resources/oauth-desktop.json"
cp "$repo/LICENSE" "$contents/Resources/Notices/MirageSSD-LICENSE.txt"
cp "$rclone_source/COPYING" "$contents/Resources/Notices/rclone-COPYING.txt"
cp "$repo/third_party/rclone-miragessd/rclone-v1.75.0.patch" "$contents/Resources/Notices/"
cp "$repo/third_party/rclone-miragessd/README.md" "$contents/Resources/Notices/rclone-miragessd-PROVENANCE.md"

echo "Signing with identity: $identity"
sign_flags=(--force --sign "$identity")
if [[ "$identity" != '-' ]]; then sign_flags+=(--options runtime --timestamp); fi
codesign "${sign_flags[@]}" --identifier org.miragessd.rclone "$contents/MacOS/rclone"
codesign "${sign_flags[@]}" "$app"
codesign --verify --deep --strict "$app" || fail 'Signature verification failed.'

echo 'Verifying package resources...'
"$contents/MacOS/MirageSSD" --check-package

mkdir -p "$output"
base="$output/MirageSSD-$version-macos-$arch"
rm -f "$base.zip" "$base.dmg"
ditto -c -k --keepParent "$app" "$base.zip"
artifacts=("$(basename "$base").zip")
if [[ "$make_dmg" -eq 1 ]]; then
  dmg_root="$stage/dmg"
  mkdir -p "$dmg_root"
  cp -R "$app" "$dmg_root/"
  ln -s /Applications "$dmg_root/Applications"
  hdiutil create -quiet -volname "MirageSSD $version" -srcfolder "$dmg_root" -ov -format UDZO "$base.dmg"
  artifacts+=("$(basename "$base").dmg")
fi
(cd "$output" && shasum -a 256 "${artifacts[@]}" > "$(basename "$base").sha256")

echo
echo "Package: $base.zip"
[[ "$make_dmg" -eq 1 ]] && echo "Disk image: $base.dmg"
cat "$base.sha256"
if [[ "$identity" == '-' ]]; then
  echo
  echo 'NOTE: ad-hoc signed. Gatekeeper will warn on download; users must right-click > Open once.'
  echo 'Pass --sign "Developer ID Application: ..." and notarize for a warning-free install.'
fi

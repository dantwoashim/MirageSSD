# macOS preview

MirageSSD on macOS mounts your application-owned Google Drive folder at `~/MirageSSD` in Finder using the same pinned, patched rclone provider as Windows, with [macFUSE](https://macfuse.github.io/) in place of WinFsp. It is an **engineering preview** with the same data-safety limits as the Windows build: keep originals until the remote copy is verified.

**[Download MirageSSD 0.1.2 preview (.dmg, Apple Silicon)](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.2-preview/MirageSSD-0.1.2-preview-macos-arm64.dmg)** · [Release notes and checksum](https://github.com/dantwoashim/MirageSSD/releases/tag/v0.1.2-preview)

Requirements: **macOS 12 Monterey or newer** (published builds are Apple Silicon only; Intel Macs [build from source](#build-the-app-yourself)), **macFUSE**, at least **25 GiB free** on the startup disk (20 GiB cache budget plus a 5 GiB floor), internet, and a Google account.

## Install

1. Install macFUSE from [macfuse.github.io](https://macfuse.github.io/). Approve its system extension in **System Settings → Privacy & Security** and restart if asked. Apple Silicon Macs must allow user-managed kernel extensions in Recovery when macFUSE prompts for it.
2. Open the `.dmg`, drag **MirageSSD.app** to **Applications**. Login startup refuses to run from Downloads or a disk image.
3. Preview builds are **ad-hoc signed, not notarized**. On first launch, right-click the app → **Open**, then confirm. Do not disable Gatekeeper system-wide. If macOS reports the app is damaged, run `xattr -d com.apple.quarantine /Applications/MirageSSD.app` and try again.
4. Click **Connect / reconnect Google**. Your browser opens Google sign-in; approve access. The token is stored in your login Keychain under `org.miragessd.drive`.
5. Click **Enable drive at login**. This writes `~/Library/LaunchAgents/org.miragessd.mount.plist` and starts the mount now. **Open mounted folder** shows it in Finder.

Files are stored in the Google account you connect using the `drive.file` scope. The volume is **not** a mirror of your whole My Drive.

## What is on disk

| Path | Contents |
| --- | --- |
| `~/MirageSSD` | Mount point. Must be empty before mounting; never a symlink. |
| `~/Library/Application Support/MirageSSD/cache` | rclone VFS write-back cache (`--vfs-cache-mode full`). Do not delete while uploads are pending. |
| `~/Library/LaunchAgents/org.miragessd.mount.plist` | Per-user startup agent, `KeepAlive` on failure with a 60 s throttle. |
| Login Keychain, service `org.miragessd.drive` | Google refresh token plus the account's `permissionId`, used to refuse reconnecting a different account onto an existing cache. |

## Uninstall

```sh
launchctl bootout gui/$(id -u)/org.miragessd.mount
umount ~/MirageSSD 2>/dev/null || true
rm ~/Library/LaunchAgents/org.miragessd.mount.plist
rm -rf /Applications/MirageSSD.app
```

Check that `~/MirageSSD` is empty and that no rclone process is running **before** removing the cache: `pgrep -fl rclone`. Cached data, the Keychain item, and cloud files are retained unless you remove them yourself (**Keychain Access** → search `miragessd`).

## Troubleshooting

- **"Install and approve macFUSE"**: `/Library/Filesystems/macfuse.fs` is missing. Reinstall macFUSE and approve the extension.
- **Mount fails immediately / agent keeps restarting**: run `/Applications/MirageSSD.app/Contents/MacOS/MirageSSD --mount` in Terminal to see the error. Common causes: `~/MirageSSD` not empty, revoked Google access, no network, or macFUSE not approved. Provider output is deliberately not logged because authorization responses contain tokens.
- **"That is a different Google account"**: the cache is bound to another account. Sign in with the original account, or move the cache aside only after confirming no uploads are pending.
- **"Unlock your login Keychain"**: the Keychain is locked or access was denied. Open **Keychain Access**, unlock *login*, and retry.
- **Slow first read of a file**: uncached data streams from Google. See the [performance table](../README.md#performance-what-to-expect).

Never paste tokens or `oauth-desktop.json` into an issue.

## Build the app yourself

The repository has no committed credentials or binaries. On macOS with Xcode Command Line Tools, Git, Go 1.25+, and macFUSE installed:

```sh
git clone https://github.com/dantwoashim/MirageSSD.git
cd MirageSSD
./scripts/build-rclone-miragessd.sh
./scripts/build-macos-app.sh --credentials ~/Downloads/desktop-oauth.json
```

The provider script checks out rclone `v1.75.0` at the pinned commit, applies the published patch, runs the `cmd/cmount` tests, and builds `v1.75.0-miragessd2` with CGO against the installed macFUSE SDK. The app script compiles `apps/mirage-macos/main.swift`, bundles the provider under `Contents/MacOS`, the OAuth Desktop-app registration under `Contents/Resources`, and upstream notices, then signs, verifies with `--check-package`, and writes `MirageSSD-<version>-macos-<arch>.{zip,dmg,sha256}` to `~/MirageSSD-Packages`.

Create the Desktop-app OAuth client as described in [Configure Google sign-in](building.md#2-configure-google-sign-in). The registration must be kept outside the checkout. Build once per architecture; the script does not produce universal binaries.

Published builds come from the `release-macos` GitHub Actions workflow, which runs on tag push or manually against an existing tag. It reads the Desktop-app registration from the `DRIVE_OAUTH_DESKTOP_JSON` repository secret, runs the two scripts above on a macOS runner, and attaches the DMG, ZIP, and `.sha256` to the release.

For a Gatekeeper-clean release, pass `--sign "Developer ID Application: <team>"` and notarize the resulting DMG with `xcrun notarytool submit --wait` followed by `xcrun stapler staple`. The default `--sign -` produces an ad-hoc signature suitable only for previews.

# MirageSSD

**Cloud capacity. Local caching. Your files, right in Explorer.**

MirageSSD exposes an application-owned folder in Google Drive as a writable Windows drive. Open files, save downloads, and copy folders through ordinary filesystem paths. Recently used data stays on the local disk; uploads run in the background.

**Engineering preview · Windows 11 x64 · macOS 12+**

[Download](#download) · [What it does](#what-it-does) · [How it works](#how-it-works) · [Get started](#get-started) · [Performance](#performance) · [Project map](#repository-map) · [Quick start guide](docs/quickstart.md)

## Download

**Windows:** [MirageSSD Setup (.exe)](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.4-preview/MirageSSD-Setup-v0.1.4-preview.exe) · [Release notes and checksum](https://github.com/dantwoashim/MirageSSD/releases/tag/v0.1.4-preview)

**macOS (Apple Silicon):** [MirageSSD 0.1.2 preview (.dmg)](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.2-preview/MirageSSD-0.1.2-preview-macos-arm64.dmg) · [Release notes and checksum](https://github.com/dantwoashim/MirageSSD/releases/tag/v0.1.2-preview) · [Install guide](docs/macos.md). Requires [macFUSE](https://macfuse.github.io/). Intel Macs: [build from source](docs/macos.md#build-the-app-yourself).

## What it does

- **A drive in This PC.** Setup prefers `M:` and chooses another available letter if needed.
- **Read and write caching.** Writes are staged locally before upload. Cached reads avoid fetching the same bytes again.
- **Automatic reconnect.** A per-user startup task supervises the mount and retries after process failure.
- **Your account, your capacity.** Sign-in uses the system browser. Reported capacity comes from the connected account's quota.
- **Windows file attributes.** A pinned rclone patch preserves hidden, read-only, system, and archive flags in a local metadata journal.
- **A single-file installer.** The builder packages the application, patched provider, checksum-verified WinFsp prerequisite, and upstream notices.
- **System tray companion.** A notification-area icon shows mounted drives, offers per-drive shortcuts and space reclaim, and warns when the service is unreachable.
- **Explorer integration.** Managed drive letters carry the MirageSSD label and icon, and right-clicking a folder offers "Keep on this device" / "Free up space".
- **Update checks.** The app checks for newer releases every six hours and links the download — it never auto-installs.
- **Safe removal.** `mirage volume remove` (or the drive card) deletes local state only, warns about pending uploads, and never touches Drive.

The drive uses storage in your own Google account. Google's `drive.file` access gives MirageSSD a dedicated application-managed folder.

## How it works

```text
Applications and Explorer
          |
          v
MirageSSD drive <--> Local disk cache <--> Google Drive
```

When you open a file, MirageSSD serves cached bytes locally and fetches missing content from Drive. When you save a file, it stages the write on your disk and uploads it in the background. A completed local copy means the data has reached the cache; upload completion means it has reached Google Drive.

The Windows mount combines a pinned rclone provider with WinFsp. The macOS app uses the same provider with macFUSE and exposes the folder in Finder. Startup supervision restores the mount when you sign in.

For large folders, the repository also includes packed archive and backup helpers. The separate Rust asset engine explores preparing a game's working set in advance, with page-level caching and session admission. Read the [architecture guide](docs/architecture.md) for the component boundaries.

## Get started

### Use an installer

Download the [Windows preview installer](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.4-preview/MirageSSD-Setup-v0.1.4-preview.exe), then:

1. Open `MirageSSD-Setup-*.exe` as your normal Windows user.
2. Choose **Install and connect Google Drive**.
3. Approve the administrator prompt if WinFsp needs installation.
4. Sign in with your own Google account.

The drive opens in Explorer when setup finishes. Copy a small folder to it, let the upload finish, and open the files from the mounted drive.

Requirements: **Windows 11 x64**, an **NTFS volume with at least 12 GiB free**, internet, and permission to install WinFsp. Installer details and troubleshooting are in the [build guide](docs/building.md) and [troubleshooting guide](docs/troubleshooting.md).

### Build your own installer

Install Rust 1.94.0, the MSVC C++ build tools, Git, Go 1.25 or newer, and WinFsp 2.1.25156.

From PowerShell:

```powershell
git clone https://github.com/dantwoashim/MirageSSD.git
cd MirageSSD
cargo build --release --locked -p mirage-cli
.\scripts\build-rclone-miragessd.ps1
```

Create a Google **Desktop app** OAuth configuration, then package the binaries:

```powershell
.\scripts\build-one-click-setup.ps1 `
  -DriveClientCredentials "$env:USERPROFILE\Downloads\desktop-oauth.json" `
  -MirageExecutable .\target\release\mirage.exe `
  -RcloneExecutable .\target\rclone-miragessd\rclone.exe `
  -OutputRoot "$env:USERPROFILE\Downloads\MirageSSD-Packages"
```

Use your downloaded configuration's actual filename and keep it outside the checkout. The [build guide](docs/building.md) covers OAuth setup, prerequisites, package verification, and optional developer components.

### macOS

The macOS preview mounts the same Drive folder at `~/MirageSSD` in Finder through [macFUSE](https://macfuse.github.io/) and the same pinned rclone provider. With Xcode Command Line Tools, Go, and macFUSE installed:

```sh
./scripts/build-rclone-miragessd.sh
./scripts/build-macos-app.sh --credentials ~/Downloads/desktop-oauth.json
```

This produces an ad-hoc-signed `MirageSSD.app` as a ZIP and DMG with checksums. Install, sign-in, uninstall, and troubleshooting steps are in the [macOS guide](docs/macos.md). Preview builds are not notarized: right-click → **Open** on first launch instead of lowering Gatekeeper.

## Performance

| Operation | What determines its speed |
| --- | --- |
| Read a fully cached file | Local storage and Windows memory cache |
| Save or copy into the drive | Local cache speed, until staging space becomes the limit |
| Read uncached data | Internet throughput, latency, and Google Drive |
| Finish uploading a large copy | Upstream bandwidth and provider limits |
| Copy thousands of small files | Metadata operations and per-file overhead |

Setup sizes the cache from available local space. Recently used files stay close to applications, while open files and pending uploads retain their staging space until they can be released. Leave room for your largest writes and let uploads finish before reclaiming local storage.

You can [prepare selected files](docs/prefetch.md) before opening them. The preparation tool supports ZIP metadata, file prefixes, and whole files within a chosen byte budget.

For games and large applications, use cloud storage for archives and restore the files to native storage for execution. Keep originals through upload and a verified restore. See [troubleshooting](docs/troubleshooting.md) for upload, cache, and recovery guidance.

Uninstall through **Settings → Apps → Installed apps → MirageSSD**. Removal checks for known pending writes and retains cached data, credentials, attribute metadata, cloud files, and shared WinFsp.

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/` | Rust CLI, Drive authentication, engine, cache, persistence, service, and IPC |
| `scripts/` | Windows and macOS setup, provider build, packaging, and optional backup helpers |
| `third_party/rclone-miragessd/` | Pinned provider patch and provenance |
| `native/winfsp-adapter/` | Native adapter for the separate immutable-repository path |
| `apps/mirage-ui/` | Experimental service-management interface |
| `apps/mirage-macos/` | macOS preview app (AppKit front end over the rclone + macFUSE mount) |
| `migrations/`, `schemas/` | Database migrations and persistent protocol formats |
| `tests/`, `fuzz/`, `tools/` | Fixtures, integration checks, fuzz targets, and test tools |
| `docs/` | Build instructions, architecture, specifications, and design decisions |

The one-click Windows installer runs the **rclone + WinFsp** writable drive. The immutable pack engine and management UI have their own development workflow.

## Further reading

- [Build and install](docs/building.md) — dependencies, authentication, and packaging.
- [macOS guide](docs/macos.md) — installation, Finder mounting, and app builds.
- [Architecture](docs/architecture.md) — storage paths and component responsibilities.
- [File preparation](docs/prefetch.md) — warm selected content before use.
- [Troubleshooting](docs/troubleshooting.md) — diagnose mount and transfer issues.
- [Contributing](CONTRIBUTING.md) — development workflow and verification.

## License

MirageSSD source is licensed under [Apache-2.0](LICENSE). Third-party components retain their own licenses. The [provider notes](third_party/rclone-miragessd/README.md) identify the exact rclone source and patch.

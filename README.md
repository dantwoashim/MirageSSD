# MirageSSD

**Your Google Drive, mounted in Windows Explorer—with a local write-back cache.**

MirageSSD exposes an application-owned folder in Google Drive as a writable Windows drive. Open files, save downloads, and copy folders through ordinary filesystem paths. Recently used data stays on the local disk; uploads run in the background.

**Status: Windows 11 x64 engineering preview.** This is not yet a production backup system or a replacement for a physical SSD.

## Download for Windows

**[Download MirageSSD Setup (.exe)](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.0-preview/MirageSSD-Setup-0.1.0-preview.exe)** · [Release notes and checksum](https://github.com/dantwoashim/MirageSSD/releases/tag/v0.1.0-preview)

Download the installer above, not GitHub's **Code → Download ZIP** (which contains source code). Run it as your normal Windows user, choose **Install and connect Google Drive**, and sign in with your own Google account. Administrator approval is needed if WinFsp must be installed.

The installer is **unsigned**. Do not disable Windows security to run it. Google OAuth is in production, so manual tester-email registration is no longer required; organization account policies may still restrict access. A fresh non-tester account and clean-PC installation have not yet been verified end to end.

[Build and install](docs/building.md) · [Architecture](docs/architecture.md) · [Troubleshooting](docs/troubleshooting.md) · [Contributing](CONTRIBUTING.md)

## What it does

- **A drive in This PC.** Setup prefers `M:` and chooses another available letter if needed.
- **Read and write caching.** Writes are staged locally before upload. Cached reads avoid fetching the same bytes again.
- **Automatic reconnect.** A per-user startup task supervises the mount and retries after process failure.
- **Your account, your capacity.** Sign-in uses the system browser. Reported capacity comes from the connected account's quota.
- **Windows file attributes.** A pinned rclone patch preserves hidden, read-only, system, and archive flags in a local metadata journal.
- **A single-file installer.** The builder packages the application, patched provider, checksum-verified WinFsp prerequisite, and upstream notices.

Files transfer to the Google account you connect. MirageSSD does not supply cloud storage. It uses Google's `drive.file` scope: the volume is **not a full mirror of every existing file in My Drive**.

## Get started

### Use an installer

Download the [Windows preview installer](https://github.com/dantwoashim/MirageSSD/releases/download/v0.1.0-preview/MirageSSD-Setup-0.1.0-preview.exe), then:

1. Open `MirageSSD-Setup-*.exe` as your normal Windows user.
2. Choose **Install and connect Google Drive**.
3. Approve the administrator prompt if WinFsp needs installation.
4. Sign in with your own Google account.

The drive opens in Explorer when setup finishes. Start with disposable files, wait for their upload, and confirm they remain readable after signing out of Windows and back in.

Requirements: **Windows 11 x64**, an **NTFS volume with at least 12 GiB free**, internet, and permission to install WinFsp. The preview installer is unsigned; do not disable Windows security to run it.

### Build your own installer

This repository contains source, not preconfigured account credentials or committed executables. Install Rust 1.94.0, the MSVC C++ build tools, Git, Go 1.25 or newer, and WinFsp 2.1.25156.

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

## Performance: what to expect

| Operation | What determines its speed |
| --- | --- |
| Read a fully cached file | Local storage and Windows memory cache |
| Save or copy into the drive | Local cache speed, until staging space becomes the limit |
| Read uncached data | Internet throughput, latency, and Google Drive |
| Finish uploading a large copy | Upstream bandwidth and provider limits |
| Copy thousands of small files | Metadata operations and per-file overhead |

**A completed copy into the drive is not proof of a completed cloud upload.** Write-back caching improves responsiveness; it does not make a slow connection faster.

Setup chooses a cache budget from available local space. That budget is a cleanup target, not unlimited staging capacity: open files and pending uploads cannot safely be evicted. Downloads can fill the local disk if upload cannot keep up.

## Data safety and current limits

- Keep originals until the remote copy and a restore have been verified. Do not use this preview as the only copy of irreplaceable data.
- Do not delete the cache while uploads are pending.
- Ordinary files on the writable drive are **not client-side encrypted by MirageSSD**. Encrypted repositories and backups are separate paths.
- Offline reads require cached content. Windows attributes are stored locally and do not automatically follow files to another PC.
- Games, launchers, databases, virtual machines, and anti-cheat components belong on native storage. Use the mount for archiving, not as a zero-lag execution guarantee.
- macOS, Linux desktop mounting, signed installers, and clean-machine certification are not delivered by this preview.

Uninstall through **Settings → Apps → Installed apps → MirageSSD**. Removal checks for known pending writes and retains cached data, credentials, attribute metadata, cloud files, and shared WinFsp.

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/` | Rust CLI, Drive authentication, engine, cache, persistence, service, and IPC |
| `scripts/` | Windows setup, provider build, packaging, and optional backup helpers |
| `third_party/rclone-miragessd/` | Pinned provider patch and provenance |
| `native/winfsp-adapter/` | Native adapter for the separate immutable-repository path |
| `apps/mirage-ui/` | Experimental service-management interface |
| `migrations/`, `schemas/` | Database migrations and persistent protocol formats |
| `tests/`, `fuzz/`, `tools/` | Fixtures, integration checks, fuzz targets, and test tools |
| `docs/` | Build instructions, architecture, specifications, and design decisions |

The writable drive uses **rclone + WinFsp**. The immutable pack engine and management UI are separate development components, not prerequisites for the one-click writable-drive installer.

## License

MirageSSD source is licensed under [Apache-2.0](LICENSE). Third-party components retain their own licenses. The [provider notes](third_party/rclone-miragessd/README.md) identify the exact rclone source and patch.

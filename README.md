# MirageSSD

**Cloud capacity. Local speed. Your files, right in Explorer.**

MirageSSD gives your Google Drive a drive letter in File Explorer on Windows. Files you use live on this PC for speed; uploads to Drive run in the background, encrypted on your PC and verified before anything is evicted.

**Engineering preview · Windows 11 x64 (macOS 12+ preview available)**

[Download](#download) · [What it does](#what-it-does) · [How it works](#how-it-works) · [Get started](#get-started) · [Known limitations](#known-limitations) · [Build from source](#build-from-source) · [Quick start guide](docs/quickstart.md)

## Download

**Windows:** grab `MirageSSD-Setup-*.exe` from the [latest release](https://github.com/dantwoashim/MirageSSD/releases/latest). It's a WiX bundle that installs WinFsp 2.1.25156 only if it is missing (shared, permanent) and the MirageSSD MSI per-machine — administrator approval is required. The preview is unsigned, so Windows may warn before running it.

**macOS (Apple Silicon):** the DMG is on the [same release page](https://github.com/dantwoashim/MirageSSD/releases/latest); see the [macOS guide](docs/macos.md). Requires [macFUSE](https://macfuse.github.io/).

## What it does

- **A drive letter in This PC.** A managed drive appears in Explorer; open files, save downloads, and copy folders through ordinary paths.
- **Local journal and cache.** Writes land in a local journal and contiguous writes are sealed into durable segment files; recently used data stays on your disk. A local budget plus an "Always keep free" floor bound local use.
- **Encrypted, verified uploads.** A background publisher uploads sealed data to the drive's app folder in Google Drive as immutable objects, encrypted on this PC with a per-drive key. Small files are packed into larger uploads so each Drive object carries hundreds of files instead of one. Uploads are read back and hash-verified before the local copy may be evicted — "a copy finished" is not the same as "upload finished".
- **Keep on this device / Free up space.** Pin folders from the app or the Explorer right-click menu so they're never evicted; free evictable bytes on demand. Eviction only removes data already uploaded and not pinned.
- **Reconnects automatically.** A per-user logon agent keeps this user's drives mounted and supplies fresh Drive access tokens to their hosts.
- **Desktop app and tray.** `mirage-ui.exe` serves the management UI on loopback with a per-launch token and opens in an Edge app window (falls back to your default browser); a notification-area companion can start at logon.
- **Capacity from your quota.** Explorer's capacity display comes from the Google storage quota of the connected account.
- **Update checks.** The app checks for newer releases every six hours and links the download — it never auto-installs.
- **Safe removal.** "Remove from this PC" deletes local state only, warns about pending uploads, and never touches Drive.

## How it works

```text
Explorer / ordinary applications
        |
      WinFsp
        |
 mirage-fs.exe          <- native WinFsp filesystem host (one per mounted drive)
        |
 Rust engine (mirage-ffi C ABI)
   local journal + cache  <->  background publisher
        |                          |
        +-------- mirage-service (Windows service: repositories, mounts, floors,
        |          Explorer icon & verbs, named-pipe IPC)
        +-------- mirage-ui (loopback desktop app) and the per-user logon agent
                                   |
                         Google Drive (app folder, immutable encrypted objects)
```

Reads of data on this PC are served locally; other data is fetched from Drive, hash-verified, and cached. See the [architecture guide](docs/architecture.md) for component boundaries.

## Get started

Requirements: **Windows 11 x64**, internet, a local **NTFS or ReFS disk** with room for the cache, and **administrator approval** to install.

1. Download `MirageSSD-Setup-*.exe` from [Releases](https://github.com/dantwoashim/MirageSSD/releases/latest).
2. Run it and approve the administrator prompt — it installs WinFsp if missing, then the MirageSSD MSI per-machine.
3. Launch MirageSSD and sign in with your own Google account (system browser, PKCE; MirageSSD only gets access to files it creates).

MirageSSD creates your drive right after sign-in; open it with **Open in Explorer** in the app. Copy a small folder to it, let the upload finish, and verify you can read it back.

## Known limitations

- **Unsigned preview** — Windows may warn before running the installer.
- **Windows file attributes** (hidden/read-only/system) are not preserved on drives; files report as normal.
- **Offline** — only data already on this PC can be read.
- **Local copies aren't encrypted** — cached and staged data on your disk is not encrypted by MirageSSD.
- **One PC only** — the per-drive encryption key is DPAPI-protected on the PC that created the drive; there is currently no supported way to open a drive from another PC or after reinstalling Windows.
- **Not a backup** — keep independent copies.

## Build from source

Rust via rustup (the pinned `rust-toolchain.toml` selects 1.94.0), Visual Studio 2022 Build Tools with C++ and the Windows SDK, CMake ≥ 3.25, Node.js ≥ 22.19, WiX 4.0.6, and WinFsp 2.1.25156 for mounted tests. The full recipe — Google sign-in configuration, packaging, and developer checks — is in [docs/building.md](docs/building.md). The five headline commands:

```powershell
cargo build --release --workspace --locked
.\scripts\acquire-winfsp-sdk.ps1 -WixExecutable .\.tools\wix\wix.exe
cmake --preset windows-msvc-release
cmake --build --preset windows-msvc-release
cd apps\mirage-ui; npm ci; npm run build
```

## Repository map

| Path | Responsibility |
| --- | --- |
| `crates/` | Rust CLI, Drive authentication, engine, cache, persistence, service, and IPC |
| `native/winfsp-adapter/` | `mirage-fs.exe` — the WinFsp host for the Windows drive |
| `installer/` | WiX MSI and setup bundle |
| `apps/mirage-ui/` | Desktop app: drives, setup, settings and help (served by `mirage-ui.exe`) |
| `apps/mirage-macos/` | macOS preview app (AppKit front end over the rclone + macFUSE mount) |
| `scripts/` | Windows and macOS setup, provider build, packaging, and optional backup helpers |
| `tools/perf/` | Mounted-drive benchmarks |
| `third_party/rclone-miragessd/` | Patched provider used by the macOS preview |
| `migrations/`, `schemas/` | Database migrations and persistent protocol formats |
| `tests/`, `fuzz/`, `tools/` | Fixtures, integration checks, fuzz targets, and test tools |
| `docs/` | Build instructions, architecture, specifications, and design decisions |

## Further reading

- [Quick start](docs/quickstart.md) — install to everyday use.
- [Build and install](docs/building.md) — dependencies, authentication, and packaging.
- [macOS guide](docs/macos.md) — installation, Finder mounting, and app builds.
- [Architecture](docs/architecture.md) — storage paths and component responsibilities.
- [File preparation](docs/prefetch.md) — warm selected content before use.
- [Troubleshooting](docs/troubleshooting.md) — diagnose mount and transfer issues.
- [Contributing](CONTRIBUTING.md) — development workflow and verification.

## License

MirageSSD source is licensed under [Apache-2.0](LICENSE). Third-party components retain their own licenses. The [provider notes](third_party/rclone-miragessd/README.md) identify the exact rclone source and patch.

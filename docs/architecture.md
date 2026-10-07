# Architecture

MirageSSD contains two storage paths with separate guarantees.

## Writable drive

This is the user-facing Windows product — the managed-drive architecture.

```text
Explorer / ordinary application
             |
           WinFsp
             |
       mirage-fs.exe            <- native WinFsp filesystem host (per mounted drive)
             |                    calls the Rust engine through the mirage-ffi C ABI
   Rust engine (mirage-ffi)
     local journal + cache  <->  background publisher  ---->  Google Drive
             |                                                (app folder,
   mirage-service (Windows service, LocalSystem)               immutable
     repositories, mounts, free-space floors,                  encrypted
     Explorer icon + right-click verbs, named-pipe IPC)        objects)
             |
   mirage-ui.exe (loopback desktop app)   mirage.exe (CLI + per-user logon agent)
```

`mirage-service.exe` owns repositories, mounts, free-space floors, the Explorer drive icon and right-click verbs, and local named-pipe IPC. `mirage-fs.exe` is the native C++ WinFsp filesystem host for each mounted drive and calls the Rust engine through the mirage-ffi C ABI. `mirage-ui.exe` serves the management UI over loopback HTTP with a per-launch token (opened as an Edge app window, falling back to the default browser) and has a `--tray` notification-area companion registered under HKCU Run. `mirage.exe` is the CLI; a per-user logon agent under HKCU Run keeps this user's drives mounted and supplies fresh Drive access tokens to their hosts.

### Reads and writes

Reads of data already on this PC are served locally; other data is fetched from Drive, hash-verified, and cached. Writes land in a local journal on this PC — contiguous writes go into segment files that a background thread seals durably. **Local completion and remote completion are different events.**

### Durability

Every operation is atomic and ordered. Closing a file does not wait for the disk — committed changes reach the disk within about a quarter second, or when an application calls `FlushFileBuffers` (which returns only once that file is durable). A crash or power loss can lose writes from roughly the last quarter second, including files whose close was still being sealed, but never leaves corrupted or half-applied state. Local payload files are deleted only after the state that allows the deletion is durable. Setting `MIRAGE_DURABILITY=strict` in the `mirage-fs.exe` environment restores per-operation durability: every commit is fsynced and close waits for the seal.

### Publication and eviction

A background publisher uploads sealed data to the drive's app folder in Google Drive as immutable objects, encrypted on this PC with a per-drive key. Payloads up to 4 MiB are uploaded together in packs of up to 16 MiB / 1,024 payloads, so many small files cost one Drive object instead of one each (Drive limits sustained write requests to about 3 per second per account); each payload stays individually encrypted and verified, and reads fetch only the byte range they need. Uploads are read back and hash-verified before the local copy may be evicted. A local budget plus a free-space floor ("Always keep free") bound local use; eviction only removes data already uploaded and not pinned, and pinned folders ("Keep on this device") are never evicted.

### Identity and lifecycle

Sign-in happens in the system browser with PKCE and a loopback callback; the scope is `drive.file` (app-created files only). Tokens are stored per user and protected with Windows DPAPI, as is the per-drive encryption key — so there is currently no supported way to open a drive from another PC or after reinstalling Windows. Volume capacity comes from the Google storage quota, not physical disk capacity.

| Component | Main implementation |
| --- | --- |
| WinFsp filesystem host (`mirage-fs.exe`) | `native/winfsp-adapter/` |
| Write-behind segments and their durable seal | `crates/mirage-ffi/src/segments.rs` |
| Writes, extent journal, and namespace mutations | `crates/mirage-ffi/src/write.rs`, `crates/mirage-ffi/src/mutation.rs` |
| Origin fetch and reads | `crates/mirage-ffi/src/read.rs` |
| Background publisher (encrypt, upload, read-back verify) | `crates/mirage-ffi/src/publisher.rs` |
| Service control plane | `crates/mirage-service/src/control_plane.rs`, `crates/mirage-service/src/control_plane/` |
| Desktop app host | `crates/mirage-ui-host/` |
| Logon agent | `crates/mirage-cli/src/commands/agent.rs` |
| OAuth and protected token storage | `crates/mirage-backend-drive/src/oauth.rs`, `crates/mirage-backend-drive/src/token_store.rs` |
| MSI and setup bundle | `installer/` |

## macOS preview (rclone + macFUSE)

The macOS preview is a separate AppKit app over the patched rclone provider and macFUSE, mounting an app folder at `~/MirageSSD` in Finder. Files upload as ordinary files (not encrypted by MirageSSD). See the [macOS guide](macos.md).

| Component | Main implementation |
| --- | --- |
| macOS app | `apps/mirage-macos/`, `scripts/build-macos-app.sh` |
| Patched provider and its build | `third_party/rclone-miragessd/`, `scripts/build-rclone-miragessd.sh` |

### Legacy Windows rclone package

Earlier Windows previews mounted the same patched provider through WinFsp. These files remain for reference; they are not the supported Windows install.

| Component | Main implementation |
| --- | --- |
| Mount command and provider options | `crates/mirage-cli/src/commands/device_drive.rs` |
| Setup and cache selection | `scripts/setup-miragessd.ps1` |
| Installation and supervision | `scripts/install-device-drive.ps1` |
| Single-file bootstrapper | `scripts/friend-setup.cs` |
| Windows attributes | `third_party/rclone-miragessd/rclone-v1.75.0.patch` |
| Removal and retention checks | `scripts/uninstall-device-drive.ps1` |

## Immutable-repository engine

The Rust engine stores versioned manifests, indexes, and packs; verifies page contents; manages residency; and coordinates sessions through a service and authenticated IPC. Its C++ WinFsp adapter exposes a separate immutable-repository filesystem.

This path contains encryption, owner binding, profiling, simulation, and session-admission work. The writable drive shares the engine's journal, cache, and encrypted publication, but the profiling and session-admission work does not establish production-ready game streaming.

The [format specifications](specs/) and [design decisions](adr/) describe its persistent contracts. Its management interface talks through a local host to the service; it does not provide the ordinary writable mount.

### Readiness compilation and the shared miss path

The engine can now compile a readiness verdict for a declared scope: `compile_readiness` prices each presentation-backend candidate in a fixed order (compatibility, spatial, source, temporal) and returns either a versioned `ReadinessRecord` or an `Unsupported` plan naming the refusing constraints — never a blended "ready". The terminology, envelope inequalities, and record shape are frozen in [readiness-v1](specs/readiness-v1.md); backend selection is governed by [ADR 0008](adr/0008-capability-selected-presentation.md). Gate-E0 platform facts are captured by the read-only inventory described in [capability-inventory-v1](specs/capability-inventory-v1.md), which records presence only — it is not a qualification by itself. Misses on the immutable path go through the shared flight described under "Shared miss path" in [engine-api](specs/engine-api.md): one owner reserves budget and fetches, subscribers share the completion, and cancellation detaches rather than stranding. Slot reuse for a sealed mounted generation is restricted as specified in [slot-lifetime-v1](specs/slot-lifetime-v1.md) until a cross-process reader lease exists. The compiler and inventory are implemented; the WinFsp host's miss path is still served from a local origin and is not yet bridged to the shared engine path, and no CFAPI/ProjFS adapter exists.

## Backup helpers

Optional CLI and PowerShell helpers integrate with Restic for encrypted packed backups and restores. They require separate configuration and recovery material, and are not installed by the Windows setup bundle. An ordinary Explorer copy is not a verified packed backup.

## Failure boundaries

Restarting a process does not repair expired authorization, missing drivers, unavailable internet, or full local storage. An offline mount cannot return data that was never cached.

Source reclamation remains a separate, explicit action. Cached-write success is never permission to delete originals.

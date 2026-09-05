# Architecture

MirageSSD contains two storage paths with separate guarantees.

## Writable drive

This is the user-facing Windows preview.

```text
Explorer / ordinary application
             |
           WinFsp
             |
     patched rclone VFS
        /           \
local NTFS cache   Google Drive
                   (user account)
```

WinFsp exposes the filesystem to Windows. The rclone VFS implements ordinary file operations, cached content, ranged reads, and background uploads. MirageSSD supplies authentication, supervision, installation, and device configuration.

### Reads and writes

Cached reads stay local; uncached reads use the network and can fail when the remote data is unavailable. Background directory prewarming loads metadata, not file contents. This path does not promise network-free metadata operations.

Writes enter the cache first. Closed files become eligible for upload after a short write-back delay. **Local completion and remote completion are different events.** Open files and pending uploads cannot be evicted simply to meet a size target, so available local space limits staging.

### Identity and lifecycle

Setup runs as the ordinary user. A per-user scheduled task supervises the hidden launcher, starts at sign-in, and retries periodically. Credentials use Windows DPAPI and owner-limited filesystem permissions. Driver installation is the privileged step.

Volume capacity comes from the connected account's quota, not physical SSD capacity. The rclone attribute patch maintains a local journal across rename and deletion; that journal is not automatically synchronized to another PC.

| Component | Main implementation |
| --- | --- |
| Desktop authorization and protected tokens | `crates/mirage-backend-drive/src/oauth.rs`, `token_store.rs` |
| Mount command and provider options | `crates/mirage-cli/src/commands/device_drive.rs` |
| Setup and cache selection | `scripts/setup-miragessd.ps1` |
| Installation and supervision | `scripts/install-device-drive.ps1` |
| Single-file bootstrapper | `scripts/friend-setup.cs` |
| Windows attributes | `third_party/rclone-miragessd/rclone-v1.75.0.patch` |
| Removal and retention checks | `scripts/uninstall-device-drive.ps1` |

## Immutable-repository engine

The Rust engine stores versioned manifests, indexes, and packs; verifies page contents; manages residency; and coordinates sessions through a service and authenticated IPC. Its C++ WinFsp adapter exposes a separate immutable-repository filesystem.

This path contains encryption, owner binding, profiling, simulation, and session-admission work. Those features do not make the writable drive an encrypted pack filesystem or establish production-ready game streaming.

The [format specifications](specs/) and [design decisions](adr/) describe its persistent contracts. Its management interface talks through a local host to the service; it does not provide the ordinary writable mount.

## Backup helpers

Optional CLI and PowerShell helpers integrate with Restic for encrypted packed backups and restores. They require separate configuration and recovery material, and are not automatically installed by the one-click package. An ordinary Explorer copy is not a verified packed backup.

## Failure boundaries

Restarting a process does not repair expired authorization, missing drivers, unavailable internet, or full local storage. An offline mount cannot return data that was never cached.

Source reclamation remains a separate, explicit action. Cached-write success is never permission to delete originals.

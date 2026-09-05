# Troubleshooting

Use disposable data while evaluating the preview. Keep originals until the remote copy and a restore are verified.

## Google blocks sign-in

For a Testing-mode OAuth app, its owner must add your exact account under **Google Auth Platform → Audience → Test users**. The client must be a **Desktop app**, with Google Drive API enabled. See [Google configuration](building.md#2-configure-google-sign-in).

Never paste login tokens into an issue.

## The drive is missing

Confirm WinFsp installed and check Task Scheduler for your user-specific `MirageSSD Drive` task. Sign out of Windows and back in to exercise startup. Check whether setup selected another drive letter.

A restarted task cannot fix revoked authorization, insufficient local space, or unavailable internet. Logs live under `%LOCALAPPDATA%\MirageSSD\logs`; inspect locally and redact account information and personal paths before sharing excerpts.

Do not format anything, delete the cache, or remove credentials as a first troubleshooting step.

## Copies start fast, then slow down

Early progress can be local cache acceptance; uploads still consume upstream bandwidth. Small files add per-file metadata overhead.

Leave local free space available. Do not start a single open download larger than available staging space. The cache ceiling cannot safely force pending data to disappear.

## Capacity differs from my physical disk

Cloud quota, local cache size, and physical free space are separate quantities. Other Google storage usage affects available quota, and decimal/binary units can differ between displays. The virtual drive does not create additional physical SSD space.

## Attributes differ on another PC

Attributes persist in the patched provider's local journal. They survive local remounts but do not automatically synchronize between machines.

## Uninstall

Use **Settings → Apps → Installed apps → MirageSSD**. Removal refuses known pending writes and retains cache, credentials, attribute metadata, remote files, and shared WinFsp. Do not purge retained data before verifying pending uploads and recovery.

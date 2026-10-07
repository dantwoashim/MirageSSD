# Troubleshooting

Use disposable data while evaluating the preview. Keep originals until the remote copy and a restore are verified.

## Google blocks sign-in

For a Testing-mode OAuth app, its owner must add your exact account under **Google Auth Platform → Audience → Test users**. The client must be a **Desktop app**, with Google Drive API enabled. See [Google configuration](building.md#2-configure-google-sign-in).

Never paste login tokens into an issue.

## The drive is missing

Open MirageSSD → **Drives** → **Connect** for the drive that should be mounted. If the service is not running, use **Help → Start service**. Make sure WinFsp is installed — the setup bundle installs it only when missing. Signing out of Windows and back in restarts the per-user logon agent that keeps your drives mounted.

Do not format anything, delete the cache, or remove credentials as a first troubleshooting step.

## Copies start fast, then slow down

Early progress can be local journal and cache acceptance; uploads still consume upstream bandwidth. Small files add per-file metadata overhead. A finished copy is not a finished upload — sealed segments are uploaded and hash-verified in the background.

Leave local free space available. Uploads in progress stay local; the cache ceiling cannot safely force pending data to disappear.

## My disk is filling up

Use **Free up space** on the drive (app or the Explorer right-click menu) to evict data already uploaded and not pinned. You can also move the cache to another disk or adjust **Always keep free** so more space stays free. Uploads in progress stay local until they complete and verify.

## Capacity differs from my disk

Explorer's capacity display comes from your Google storage quota — not your physical disk. Other Google storage usage affects available quota, and decimal/binary units can differ between displays.

## Collecting diagnostics

In the app use **Settings** or **Help → Collect diagnostics** (also available from the tray menu). Logs live under `%ProgramData%\MirageSSD\logs` (service) and `%LOCALAPPDATA%\MirageSSD\logs` (per-user components); the service database is under `%ProgramData%\MirageSSD`. Logs can contain paths, filenames, and error details — review and redact before sharing.

## Uninstall

Use **Settings → Apps → Installed apps → MirageSSD**. Uninstall keeps `%ProgramData%\MirageSSD`, `%LOCALAPPDATA%\MirageSSD`, and everything in your Google Drive — remove the app folder from drive.google.com if you want it gone.

---

macOS issues → see [docs/macos.md](macos.md).

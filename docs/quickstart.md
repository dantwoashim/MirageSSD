# MirageSSD quick start

MirageSSD gives you a normal Windows drive letter backed by your Google Drive.
Files you save land in a local journal on this PC and are uploaded — encrypted
and hash-verified — in the background; data already uploaded can be evicted
from the local copy and fetched back on demand.

## Install

Run `MirageSSD-Setup-v<version>-preview.exe`. It installs the WinFsp filesystem
driver (only if it is missing) and the MirageSSD MSI per-machine — administrator
approval is required. When setup finishes, choose **Launch** to open the
MirageSSD window.

## Sign in and your first drive

The first launch opens the setup wizard. Click **Sign in with Google** and
finish sign-in in the browser window that opens. MirageSSD asks only for
access to files it creates — it cannot see the rest of your Drive. Your
credentials stay on this PC, encrypted for your Windows account.

The drive is then created automatically with defaults — a free drive letter, a
name, and cache limits sized from your free space. When you add further drives
you can change the letter, name, and the **Cache location and limits**:

- **Local SSD budget** — how much disk MirageSSD may use to keep files fast
  locally. Bigger means more of your Drive is instant; the default is 25% of
  your freest disk, capped at 64 GiB.
- **Always keep free** — a free-space floor on that disk. If free space ever
  drops below it, MirageSSD evicts cloud-backed copies of files you already
  synced before it lets anything else use the disk.

After a restart or sign-out, a small logon agent signs back in and reconnects
the drive on its own.

## Where files live

- In **Explorer**: the drive letter you picked. Everything under it is a
  normal file from Windows' point of view.
- In **Google Drive**: a MirageSSD app folder containing encrypted,
  content-addressed objects — not the original filenames or contents.
- **Locally**: staged data under `%LOCALAPPDATA%\MirageSSD`, bounded by the
  budget you set.

Every upload is read back from Drive and hash-verified before MirageSSD will
evict the local copy — so a file is never dropped locally until Drive provably
has it.

## Everyday use

- **Drives page.** Open a drive **in Explorer**, **Disconnect**/**Connect**,
  **Keep folders on this PC** (pin folders so they're never evicted), **Move a
  folder into this drive**, **Free up space**, or **Remove from this PC**
  (deletes only local state — the Drive folder is never touched; pending
  uploads are flagged first).
- **Settings.** Account and sign-out, theme, update status and download, and
  Collect diagnostics.
- **Help.** Service status, common fixes, and diagnostics.
- **Tray.** Right-click the notification-area icon: **Open MirageSSD**, a
  per-drive **Open X:**, **Free up space now**, **Account…**,
  **Collect diagnostics**, **Quit**. The tray can start at logon.
- **Updates.** The app checks GitHub releases at most every six hours and
  shows a banner with a download link; nothing auto-downloads.

## Removing MirageSSD

Uninstall from **Settings → Apps** (or Programs and Features). Your Google
Drive copy is untouched, and the service's database in
`C:\ProgramData\MirageSSD` is preserved so reinstalling finds your volumes.
To also erase local staging and credentials, delete `%LOCALAPPDATA%\MirageSSD`
after uninstalling. Nothing is ever deleted from your Drive folder — remove it
from drive.google.com if you want it gone.

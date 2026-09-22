# MirageSSD quick start

MirageSSD gives you a normal Windows drive letter backed by your Google Drive.
Files you save are staged on a fast local disk and uploaded in the background;
things you haven't touched in a while can be evicted from the local copy and
fetched back on demand.

## Install

Run `MirageSSD-Setup-<version>.exe`. It installs the WinFsp filesystem driver
(if needed), the MirageSSD background service, and the desktop app. When setup
finishes, choose **Launch** to open MirageSSD.

## Sign in

The first launch opens the welcome wizard. Click **Sign in with Google** and
finish sign-in in the browser window that opens. MirageSSD asks only for
access to files it creates — it cannot see the rest of your Drive. Your
credentials stay on this PC, encrypted for your Windows account.

## Create your drive

Step 2 asks for:

- **Drive letter** — the letter the drive appears under in File Explorer
  (first free letter from M: is suggested).
- **Name** — the label shown in Explorer.
- **Local SSD budget** — how much disk MirageSSD may use to keep files fast
  locally. Bigger means more of your Drive is instant; the default is 25% of
  your freest disk, capped at 64 GiB.
- **Always keep free** — a free-space floor on that disk. If free space ever
  drops below it, MirageSSD evicts cloud-backed copies of files you already
  synced before it lets anything else use the disk.

Click **Create drive**. MirageSSD makes a fresh folder in your Google Drive,
creates the empty volume, mounts it, and remembers it — after a restart or
sign-out, a small logon agent signs back in and reconnects the drive on its
own.

## Where files live

- In **Explorer**: the drive letter you picked. Everything under it is a
  normal file from Windows' point of view.
- In **Google Drive**: a MirageSSD folder per volume containing encrypted,
  content-addressed objects — not the original filenames or contents.
- **Locally**: staged data under the volume's state directory inside
  `%LOCALAPPDATA%\MirageSSD`, bounded by the budget you set.

Every upload is read back from Drive and hash-verified before MirageSSD will
evict the local copy — so uploads use roughly twice the bandwidth of the file
itself, and a file is never dropped locally until Drive provably has it.

## Removing MirageSSD

Uninstall from **Settings → Apps** (or the bundle's entry in Programs and
Features). Your Google Drive copy is untouched, and the service's database in
`C:\ProgramData\MirageSSD` is preserved so reinstalling finds your volumes.
To also erase local staging and credentials, delete `%LOCALAPPDATA%\MirageSSD`
after uninstalling. Nothing is ever deleted from your Drive folder — remove it
from drive.google.com if you want it gone.

# Installer contract

MirageSSD is a per-machine x64 installation. It installs versioned binaries and a LocalSystem coordination service, requires Windows 11 and an existing supported WinFsp runtime, creates no repository or game mount, and treats all repository/game data as user-owned retained data.

Installation failure must roll back MSI-owned binaries and service registration. Uninstall is blocked while a mount, session, or update journal is active. Removing the application never means deleting a remote repository. Release packaging must sign every executable and the final MSI, then verify those signatures on a clean machine.

# Installer contract

MirageSSD is a per-machine x64 installation. It installs versioned binaries and a LocalSystem coordination service, requires Windows 11 and an existing supported WinFsp runtime, creates no repository or game mount, and treats all repository/game data as user-owned retained data.

Installation failure must roll back MSI-owned binaries and service registration. Uninstall is blocked while a mount, session, or update journal is active. Removing the application never means deleting a remote repository. Release packaging must sign every executable and the final MSI, then verify those signatures on a clean machine.

Major upgrades remove the previous product after `InstallInitialize`, inside the
rollback transaction. Restart Manager handles in-use files. Service shutdown,
orphan registration removal, installation, and startup use the standard MSI
service actions with `Wait="yes"`; external `net`, `taskkill`, and `sc` cleanup
must not run before validation. In particular, do not ignore a failed shutdown
and mark a still-running service for deletion before trying to install it again.
The ordinary uninstall safety guard excludes `UPGRADINGPRODUCTCODE` so an upgrade
can retain existing repositories and mounts for recovery by the replacement.

`installer/build.ps1` checks the compiled MSI with
`scripts/test-msi-upgrade-contract.ps1`. This verifies action ordering and service
control flags; it does not replace an elevated install/upgrade/rollback test on a
disposable Windows machine with a running service and open application.

Windows CI additionally runs `scripts/test-msi-upgrade.ps1` on its disposable,
elevated runner. It installs a lower-version fixture, forces a deferred failure
after service installation to verify rollback to the running baseline, then
upgrades successfully and uninstalls while checking retained data. The test
refuses to run where MirageSSD or its control database already exists. It uses
the same binaries in both packages to isolate installer lifecycle behavior;
it does not certify every historical binary or an upgrade with active mounts.

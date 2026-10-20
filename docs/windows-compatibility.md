# Windows compatibility

MirageSSD 0.1.22 targets **Windows 10 22H2 x64 (build 19045) and Windows 11 x64**.
The same installer and binaries serve both. Windows 10 21H2 and earlier, 32-bit
Windows and native ARM64 builds are outside this target. Windows Server has not
been qualified. Keep the OS and browser updated under their applicable support
programs; accepting an OS does not extend Microsoft's support lifecycle.

The earlier MSI rejected every build below 22000. The MSI now accepts build
19045 or newer on x64, and the setup bundle checks before installing WinFsp.
Installed-product maintenance remains possible independently of the fresh-install
gate. The legacy PowerShell setup uses the registry's `CurrentBuildNumber` rather
than manifest-dependent `Environment.OSVersion`. Windows PowerShell 5.1 is enough
to run installed scripts. Build tools have their own prerequisites.

The baseline deliberately selects Windows 10's final feature release rather than
claiming every Windows 10 release. Rust's MSVC target supports Windows 10 and
later ([Rust platform support](https://doc.rust-lang.org/rustc/platform-support.html));
the pinned WinFsp 2.1 runtime supports Windows 10
([WinFsp](https://github.com/winfsp/winfsp)). The native adapter compiles with
`WINVER` and `_WIN32_WINNT` set to Windows 10. These SDK macros are compile-time
guards, not a substitute for running the binaries on the target OS.

The desktop host launches Edge with `--app` and falls back to the default browser.
It has no WebView2 installation dependency. A current Edge, Chrome or Firefox
installation is needed for the management UI. Native filesystem, service, process,
DPAPI and shell integration use Windows 10 APIs; no Windows 11-only API was found
in the compatibility review. Local NTFS/ReFS cache, administrator-approved setup
and the pinned signed WinFsp runtime are still required. Runtime DLLs are deployed
app-local by the MSI.

## Qualification

The packaging contract checks both installer gates and release metadata. For a
compiled MSI, `scripts/test-windows-platform-contract.ps1 -MsiPath <file>` evaluates
its actual condition for Windows 10 21H2 rejection, 22H2 acceptance, Windows 11
acceptance, 32-bit rejection and installed-product maintenance. It opens the MSI
read-only and never runs an installation sequence. These boundary tests also run
from `installer/build.ps1`.

No Windows 10 machine or VM was available for this change. Windows 11 build/test
results and installer-condition tests establish build and packaging compatibility;
they do **not** establish a completed Windows 10 runtime acceptance test. Before
calling a release Windows 10-qualified, use a disposable Windows 10 22H2 x64 VM:

1. Install the setup EXE on a clean machine without developer tools; confirm
   WinFsp, service startup, UI and tray work, and restart Windows.
2. Sign in through the system browser, create a disposable repository, write/read
   exact bytes, rename/delete, unmount/remount, and verify remote restoration.
3. Exercise managed-game fixture handoff, exact reads and post-exit cleanup;
   actual title gameplay remains a separate publisher/anti-cheat acceptance test.
4. Test upgrade rollback and ordinary uninstall without deleting retained user
   data; test an unsupported-build VM rejects setup before installing WinFsp.

The local 0.1.22 candidate is unsigned and unpublished. Its provenance and hashes
are kept alongside the candidate, separately from existing published artifacts.

For this candidate on Windows 11, formatting, clippy, native debug build, 65 UI
tests and the three-file UI build passed. The complete workspace run recorded
733 passed, two failed and 14 ignored. The two failures were the existing
managed-game process-identity and monitoring-timeout fixtures; they remain
unresolved in this compatibility change. Compiled MSI boundary checks passed
under Windows PowerShell 5.1, and the extracted setup bootstrapper contained
the intended build-19045 condition. These results do not qualify Windows 10
runtime behavior or remove the existing managed-game acceptance limitations.

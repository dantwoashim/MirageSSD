# Build and install

The shipping Windows product is the managed MirageSSD installer — a WiX bundle that installs WinFsp 2.1.25156 if it is missing and the MirageSSD MSI per-machine. An earlier rclone-based Windows package is kept for reference only (see [Legacy Windows rclone package](#legacy-windows-rclone-package)).

## Prerequisites

Windows 11 x64 with PowerShell and Git, plus:

- Rust through rustup; `rust-toolchain.toml` selects 1.94.0.
- Visual Studio 2022 Build Tools, **Desktop development with C++**, and the Windows SDK.
- CMake 3.25 or newer.
- Node.js 22.19 or newer.
- WiX 4.0.6 in `.tools\wix` (`dotnet tool install wix --tool-path .tools/wix --version 4.0.6`).
- [WinFsp 2.1.25156](https://github.com/winfsp/winfsp/releases/tag/v2.1) installed for mounted-volume tests. `installer\build-setup.ps1` downloads and checksum-verifies the WinFsp MSI it bundles.

## 1. Build

```powershell
git clone https://github.com/dantwoashim/MirageSSD.git
cd MirageSSD
cargo build --release --workspace --locked
.\scripts\acquire-winfsp-sdk.ps1 -WixExecutable .\.tools\wix\wix.exe
cmake --preset windows-msvc-release
cmake --build --preset windows-msvc-release
```

UI assets:

```powershell
cd apps\mirage-ui
npm ci
npm run build
```

Outputs: `target\release\{mirage,mirage-service,mirage-ui}.exe`, `build\windows-msvc-release\native\winfsp-adapter\Release\mirage-fs.exe`, and `apps\mirage-ui\dist\` (exactly `index.html`, `assets\mirage-ui.js`, `assets\mirage-ui.css`).

The acquire script fetches the pinned WinFsp SDK payload used by the C++ adapter build; it needs the WiX executable path to extract it.

## 2. Configure Google sign-in

The installer builder does this once. Recipients only sign in with their own Google accounts.

1. Select a project in [Google Cloud Console](https://console.cloud.google.com/).
2. Under **APIs & Services → Library**, enable **Google Drive API**.
3. Configure Google Auth Platform branding and audience.
4. Add `https://www.googleapis.com/auth/drive.file` under **Data Access**.
5. Create an OAuth client of type **Desktop app** under **Clients** and download its JSON.
6. If the app is in Testing, add each tester under **Audience → Test users**.

Keep the JSON outside the checkout. Do not use a service-account key or Web application client.

Authorization uses the system browser, PKCE, and a loopback callback. See Google's [desktop OAuth documentation](https://developers.google.com/identity/protocols/oauth2/native-app) and [Drive scope reference](https://developers.google.com/workspace/drive/api/guides/api-specific-auth). The `drive.file` scope does not grant a full account-wide file mirror.

Testing-mode authorization can expire and require sign-in again. A private tester configuration is not a publicly approved OAuth app.

## 3. Package

Stage `target\release\mirage.exe`, `target\release\mirage-service.exe`, `target\release\mirage-ui.exe`, and `build\windows-msvc-release\native\winfsp-adapter\Release\mirage-fs.exe` into one folder, then:

```powershell
.\installer\build.ps1 -BinDir <stage> -DriveClientCredentials <oauth.json>
.\installer\build-setup.ps1 -MsiPath <out>\MirageSSD.msi
```

`build.ps1` produces `MirageSSD.msi` (per-machine; ships the MSVC runtime DLLs app-local so no separate VC++ redistributable is needed); `build-setup.ps1` wraps it with the pinned WinFsp MSI into `MirageSSD-Setup-<tag>.exe`. Version defaults to the `[workspace.package]` version in `Cargo.toml` and the tag to `v<version>-preview`; pass `-Version`/`-Tag` to override.

Tagged release builds use `scripts\build-release.ps1` with a clean checkout of the exact version tag.

The preview is unsigned. The bundle's license page points at the repository LICENSE; Windows may warn before running the EXE.

## Developer checks

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

On Windows the workspace suite includes a real mounted-volume test that needs WinFsp installed and the **debug** adapter built (`cmake --preset windows-msvc-debug` + `cmake --build --preset windows-msvc-debug`).

UI checks (in `apps\mirage-ui`):

```powershell
npm ci
npm test
npm run build
```

The UI build must emit exactly `index.html`, `assets\mirage-ui.js`, `assets\mirage-ui.css` — the host serves only those under a strict CSP.

To exercise the UI without a running service:

```powershell
npx vite --host 127.0.0.1 --port 5173
```

then open `/demo.html?scenario=fresh|drive|uploading|multi|offline` (`&theme=light|dark`, `&view=settings|help|setup`); mock data lives in `src/dev/mockBridge.ts`.

Mounted-drive benchmarks live in `tools\perf`. With the release adapter built:

```powershell
$env:MIRAGE_FS_EXE = 'D:\...\build\windows-msvc-release\native\winfsp-adapter\Release\mirage-fs.exe'
cargo test --release --locked -p mirage-ffi --test bench_mounted -- --ignored --nocapture
```

or `.\tools\perf\run-managed-bench.ps1 -Target <mounted-root>` against any mounted drive. Results land in `target\perf\<timestamp>\summary.json` — see `tools\perf\README.md`.

Fresh-install regression coverage includes an empty service database, a real Drive-capable WinFsp host, first-file write/read, unmount/remount, and refusal to mount after removal of the repository key. It uses disposable local metadata without a Google bearer token; it does not prove remote upload or a new user's Google OAuth consent configuration.

## macOS

The macOS preview app is built with `scripts/build-rclone-miragessd.sh` and `scripts/build-macos-app.sh` on a Mac with Xcode Command Line Tools, Go, and macFUSE. It reuses the Google configuration from step 2. See the [macOS guide](macos.md).

## Legacy Windows rclone package

**Reference only — not the supported Windows install.** The earlier package mounts the drive through a pinned rclone provider and WinFsp. Its scripts remain in the repository: `scripts/build-one-click-setup.ps1`, `scripts/friend-setup.cs`, `scripts/setup-miragessd.ps1`, and `scripts/install-device-drive.ps1`. The provider build (`scripts/build-rclone-miragessd.ps1`) checks out a fixed rclone commit, applies the published patch, tests `cmd/cmount`, and builds `v1.75.1-miragessd3`; see `third_party/rclone-miragessd/README.md`. Go 1.25 or newer is required for that provider build only.

# Build and install

The end-user path is the Windows writable-drive preview. The native immutable-repository adapter and management UI are optional, separate components.

## Prerequisites

Use Windows 11 x64 with PowerShell, Git, and:

- Rust through rustup; `rust-toolchain.toml` selects 1.94.0.
- Visual Studio 2022 Build Tools, **Desktop development with C++**, and the Windows SDK.
- Go 1.25 or newer, as specified by the [pinned provider](https://github.com/rclone/rclone/blob/9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048/go.mod).
- [WinFsp 2.1.25156](https://github.com/winfsp/winfsp/releases/tag/v2.1). Packaging requires its installed license file.
- Windows .NET Framework 4.x, including its C# compiler, for the setup wrapper.

Allow space for compiler outputs and provider dependencies. The installer's 12 GiB minimum is an end-user cache requirement, not a build-space estimate.

## 1. Build the binaries

```powershell
git clone https://github.com/dantwoashim/MirageSSD.git
cd MirageSSD
cargo build --release --locked -p mirage-cli
.\scripts\build-rclone-miragessd.ps1
```

Outputs: `target/release/mirage.exe` and `target/rclone-miragessd/rclone.exe`.

The provider script checks out a fixed rclone `v1.75.0` commit, applies the published patch, tests `cmd/cmount`, and builds `v1.75.0-miragessd2`. An ordinary rclone binary is not a substitute.

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

## 3. Package an installer

```powershell
.\scripts\build-one-click-setup.ps1 `
  -DriveClientCredentials "$env:USERPROFILE\Downloads\desktop-oauth.json" `
  -MirageExecutable .\target\release\mirage.exe `
  -RcloneExecutable .\target\rclone-miragessd\rclone.exe `
  -OutputRoot "$env:USERPROFILE\Downloads\MirageSSD-Packages"
```

Replace the configuration filename with your actual download. Output must be outside the source repository.

The builder produces an EXE, ZIP, and extracted package. Send the EXE to testers. It includes the desktop application registration, **not a user's access token, refresh token, or files**. Use an app registration you intend to distribute.

The package includes file checksums, the pinned WinFsp MSI, upstream licenses, provider source, and the patch. The preview is unsigned: checksums establish integrity, not publisher identity.

Verify without installing, substituting the actual EXE path:

```powershell
$installer = "$env:USERPROFILE\Downloads\MirageSSD-Packages\MirageSSD-Setup-TIMESTAMP.exe"
$report = "$env:TEMP\miragessd-package-check.txt"
$process = Start-Process -FilePath $installer `
  -ArgumentList ('--verify-only "' + $report + '"') -Wait -PassThru
Get-Content -LiteralPath $report
if ($process.ExitCode -ne 0) { throw 'Package verification failed.' }
```

This checks payload integrity and prerequisites. It does not authenticate, install, or test a mounted filesystem.

## 4. Install and test

Run the installer as the intended ordinary Windows user. Only driver installation requests administrator approval.

Setup chooses an available drive letter and NTFS cache location, completes sign-in, and registers mount supervision. Cache targets range from 8 to 128 GiB according to free space, with a separate free-space reserve.

Use disposable files. Check copy, edit, reopen, rename, attributes, and reconnect after Windows sign-out/sign-in. Verify a completed upload and restored copy before considering source deletion.

## Optional developer components

For Rust checks:

On Windows, the full workspace suite includes a real mounted-volume test.
Install WinFsp and build the native adapter using the commands below before
running `cargo test`. Formatting and Clippy do not need a mounted filesystem.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For the UI, use Node.js 22.19 or newer:

```powershell
cd apps/mirage-ui
npm ci
npm test
npm run build
```

The interface requires its local host and service backend. Vite alone is not a functioning storage service.

For the native adapter, return to the repository root. Use CMake 3.25 or newer, Visual Studio 2022, and WiX 4.0.6:

```powershell
dotnet tool install wix --tool-path .tools/wix --version 4.0.6
.\scripts\acquire-winfsp-sdk.ps1 -WixExecutable .\.tools\wix\wix.exe
cmake --preset windows-msvc-debug
cmake --build --preset windows-msvc-debug
```

Tagged MSI builds use `scripts/build-release.ps1` with a clean checkout and exact version tag. That is separate from the one-click writable-drive package.

## macOS

The macOS preview app is built with `scripts/build-rclone-miragessd.sh` and `scripts/build-macos-app.sh` on a Mac with Xcode Command Line Tools, Go, and macFUSE. It reuses the Google configuration from step 2. See the [macOS guide](macos.md).

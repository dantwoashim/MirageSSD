# WinFsp dependency

The native adapter is pinned to the signed WinFsp 2.1.25156 runtime and SDK ABI. CMake requires an explicit `WINFSP_SDK_ROOT`; production and tests load `winfsp-x64.dll` from the installed SxS runtime directory so the DLL selects the matching signed driver. Test-signing and private drivers are forbidden.

The supported build uses Visual Studio 2022 x64 and Windows SDK 10.0.26100. The adapter is read-only and dynamically linked to WinFsp; the Rust engine is linked as a static C ABI library.

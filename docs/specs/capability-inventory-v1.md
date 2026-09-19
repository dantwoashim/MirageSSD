# Capability inventory v1

`scripts/capability-inventory.ps1` is the gate-E0 read-only inventory. It records platform facts and changes nothing: no feature enabling, no sync-root registration, no placeholder creation, no policy changes, and no elevation prompts — it runs as the current user, and any probe that needs privileges it lacks records `status: "unavailable"` with the error text.

Fields (`schema_version: 1`):

- `captured_at_utc`, `machine`, `user_is_admin`
- `os`: `caption`, `version`, `build_number` (CIM `Win32_OperatingSystem`), `ubr` and `display_version` (read-only `CurrentVersion` registry)
- `cfapi`: `dll_present` (`System32\cldapi.dll`), plus `hresult`, `build_number`, `revision_number`, `integration_number`, and `integration_number_hex` from `CfGetPlatformInfo`
- `projfs`: `install_state` and `meaning` for the `Client-ProjFS` optional feature (1 enabled, 2 disabled, 3 absent), and `prjflt_service_present`
- `winfsp`: `install_dir` and `version` from the WinFsp registry keys, and `WinFsp.Launcher` service status
- `volume`: filesystem type, drive letter, size, free bytes, allocation unit, disk bus type/model, and physical media type for `-Path`
- `bypassio`: verbatim `fsutil bypassIo state` output and exit code — a state query, never an enable
- `notes`: two fixed strings restating that presence is not behavior and that the inventory changed nothing

Inventory results are inputs to `BackendCapability.qualified` decisions only after E1/E2 experiments; presence alone is never a qualification.

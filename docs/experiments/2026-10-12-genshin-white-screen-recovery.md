# Genshin white-screen startup recovery

The follow-up to the lag fix found a separate availability failure. At inspection,
the Python asset origin was not listening on its localhost port, and the main
rclone mount process was absent. The video mount alone was still running. The
game's asset folders remained junctions to those mounts.

The game log showed a failed external resource-index load, followed by missing
streaming files `31049740.blk` and `31049741.blk`, then repeated initialization
`NullReferenceException` messages. It also identified `11790361.blk` as unavailable.
Graphics initialization had succeeded. The missing asset path is a demonstrated
startup problem; graphics settings were not changed.

The earlier successful cached-read checks did not establish that the helpers
would remain available for a later launch. The exit cause of the absent helpers
was not captured, so maintenance-process lifetime is an inference rather than
a logged termination diagnosis.

## Changes applied

- Restored the origin and main mount; namespace checks again passed for 1,446
  files in `blocks/00`, 16 block directories, and 275 standalone videos.
- Changed `stream-root\start-genshinstream.ps1` to start helpers through the
  Windows process provider with hidden/no-console windows and the requested
  job-breakaway flag. The new helpers reported parent PID 360 after being
  created through that provider; the parent was already absent at a later
  inspection while both helpers continued serving. Existing healthy mounts
  are reused.
- Registered the checked startup script under the current user's Windows
  sign-in startup entries. No administrator or security-policy change was made.
- Added `stream-root\launch-genshin-checked.ps1` and the desktop shortcut
  **Genshin - checked startup**. It ensures both asset mounts are available and
  reads the three known startup files before opening the official HoYoPlay
  launcher. A failed preflight reports an error instead of launching the game
  against missing asset paths.
- Preserved the earlier cache/admission and prefetch fixes.

The startup script and shortcut were checked with Windows PowerShell, the runtime
used by Windows sign-in. The startup script is idempotent and reported the origin
and both mounts already healthy on the verification run.

## Content verification

The following files were read completely through the actual game paths and
matched both manifest length and full-file MD5:

| Startup asset | Verified bytes |
|---|---:|
| `11790361.blk` | 10,530,585 |
| `31049740.blk` | 36,674,845 |
| `31049741.blk` | 2,464,870 |

The checks are saved in `stream-root\run\startup-assets-verified.json`.
The current startup-entry value and previous value are preserved in
`stream-root\run\startup-registration-20261012.json`. The old startup script
is preserved as `start-genshinstream.before-persistence-fix-20261012.ps1`.

An `IsProcessInJob` diagnostic was initially treated as if any job membership
would imply ownership by the maintenance shell. That assertion was too broad:
Windows/provider job membership is not a caller-job identity. No claim that
the helpers belong to no Windows job follows from these checks. The recorded
parent process and the sign-in/checked-launch paths are the relevant lifecycle
changes. The Windows process-provider contracts are documented by Microsoft:
[process creation](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/create-method-in-class-win32-process),
[startup flags](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-processstartup).

The missing mount and startup bytes have been restored and verified. Reaching
the actual title/login screen still requires a fresh launch through HoYoPlay;
the user was asked to report that result. No login, gameplay replay, graphics
driver change, game-file fabrication, or anti-cheat alteration was performed.

## Available space

`removed-game-root` no longer existed at this inspection and D: reported approximately
139 GiB free. The previous complete-removal request remains only partly verified:
the `launcher-root` launcher folder was still present. No launcher-removal
claim is made here.

Later the game was relaunched and entered its 2,580-file integrity check.
The slow verification and its cache-retention fix are recorded in
[the verification experiment](2026-10-12-genshin-verification-cache.md).
This still does not establish successful gameplay or completion of the full
integrity check.

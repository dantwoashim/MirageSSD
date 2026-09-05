# Source inventory and file classification

`mirage repo scan` performs metadata-only traversal. It uses symlink metadata, never follows reparse
points, sorts a canonical slash-form logical inventory, records size/attributes/timestamps/extension,
reparse evidence and hard-link count where the host exposes it, and rejects unsafe paths and Windows
case collisions. The report must be outside the source root and is written through a flushed
same-directory temporary file plus rename. Source files are never opened for writing.

Classification is deny-by-default. Executable, library, script, driver, installer, anti-cheat and
configuration extensions stay native. Reparse points, directories, unknown extensions, and small
files stay native. Only explicitly configured large asset/container extensions become virtualization
candidates; this is a candidate report, not permission to delete or replace source data.

Default mandatory-native extensions are `exe`, `dll`, `sys`, `com`, `bat`, `cmd`, `ps1`, `msi`,
`cpl`, and `scr`. Default configuration extensions are `ini`, `cfg`, `json`, `toml`, `xml`, `yaml`,
and `yml`. Rules are deterministic and extension matching is ASCII case-insensitive.

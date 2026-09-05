# IPC v1

MirageSSD uses a one-mebibyte maximum little-endian length-prefixed frame containing a strict versioned request or correlated response. Commands are a closed typed set; no command accepts an arbitrary executable or unrestricted path. The named-pipe server must authenticate the Windows client token and apply the command authorization matrix before dispatch.

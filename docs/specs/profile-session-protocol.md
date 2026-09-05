# Profile-only session protocol

The launcher must canonicalize beneath the selected game root. MirageSSD starts the bounded kernel file-I/O session before creating the process, passes arguments directly without a command shell, waits for the launcher, applies a bounded drain interval, stops ETW through RAII, and records explicit version/configuration labels and loss counters. It never injects, patches, virtualizes, or mutates game files during profiling.

# MirageSSD service configuration v1

The service reads strict TOML with `format_version = 1`. Every configuration structure rejects
unknown fields. Byte quantities are unsigned byte counts; omitted cache, scheduler, and telemetry
sections use the safe defaults encoded in `mirage-config`.

`program_data_root` is mandatory and validated syntactically under an explicit Windows or Unix
policy. Windows roots must be absolute drive-letter paths and may not use UNC/device namespaces,
alternate streams, invalid component characters, or `.`/`..`. Unix roots must be absolute and may
not contain `.`/`..`. Validation does not create or probe the path.

The cache page size is a bounded power of two. Budget and arena shards must divide into fixed page
slots, the derived slot count must fit `u32`, and checked arithmetic proves that metadata plus update
reserves leave at least one usable page. Scheduler concurrency, fetch windows, and telemetry
retention are bounded.

The model has no credential-bearing fields. Only the closed `MIRAGE_DEV_*` allowlist can override
developer diagnostics; an unknown variable with that prefix is rejected. Overrides are applied
atomically and revalidated. Debug output redacts the program-data path, and file-loading errors do
not echo the supplied configuration path.

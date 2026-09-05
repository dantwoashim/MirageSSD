# Read-only filesystem compatibility

Supported: case-insensitive lookup with preserved names, stable file IDs and sizes, bounded marker enumeration, synchronous resident reads, pending completion, cancellation-safe lifetime, shared read handles, mapped reads, non-cached aligned reads, and overlapped reads.

Explicitly unsupported: writes, creates, overwrites, deletes, renames, alternate data streams, reparse points, sparse files, writable mappings, and pretending that the volume implements all NTFS metadata. Unsupported mutations fail with `STATUS_MEDIA_WRITE_PROTECTED`; namespace/type errors use precise statuses.

# Handoff deviations

- **Write-behind segment writer (2026-09):** managed-volume writes are
  committed by a background sealer thread in ≤32 MiB segments rather than
  synchronously per `mirage_write` call. Seal points: `FlushFileBuffers`,
  close of a write-capable handle, segment roll-over, 2 s idle, truncate
  (drain), delete (discard), engine quiesce/destroy. A crash can lose the
  unsealed tail of an open segment but never leaves a durable extent version
  referencing an unsealed payload; orphan `.payload` files are swept at
  mount. `byte_extents(volume_id, payload_id)` gained a partial index
  (migration 0027) so payload lookups stop scanning the table.

# Cache arena v1

Each repository-neutral shard begins with one 4096-byte checksummed header followed by fixed-size page slots. The header freezes format version, page size, and slot count. Companion 64-byte metadata records carry a monotonically increasing generation, checksummed state, page hash, and logical tail length. Slot offsets are `4096 + slot_index * page_size` with checked arithmetic.

| State | Payload readable | Evictable | Meaning |
|---|---:|---:|---|
| Free | no | no | deallocated and reusable |
| Reserved | no | no | insertion owns the slot |
| Resident | yes | yes unless pinned/leased | verified bytes and DB mapping committed |
| Evicting | no new leases | no | mapping removed, deallocation in progress |
| RetryDeallocate | no | no | deallocation failed and must retry |

Crash protocol: reserve commits `Reserved`; payload write remains invisible; flush makes bytes durable but still invisible; rehash must match; one DB transaction installs the hash mapping and `Resident`; only then can lookup expose it. A crash before that transaction recovers the slot as unreadable and deallocates it. Eviction first prevents leases, transactionally removes lookup and records `Evicting`, deallocates the exact range, then commits `Free`. A crash or failure after lookup removal recovers as `Evicting`/`RetryDeallocate`, never readable or reusable until deallocation succeeds.

Physical budget equals arena allocated ranges, companion metadata, database and journal allowance, plus filesystem reserve. Logical sparse length is not counted as physical usage. Admission must retain the configured reserve and cannot rely on eventual eviction.

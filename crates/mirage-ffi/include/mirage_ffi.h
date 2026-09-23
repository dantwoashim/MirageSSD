#ifndef MIRAGE_FFI_H
#define MIRAGE_FFI_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef struct MirageEngineHandle MirageEngineHandle;
typedef struct MirageFileHandle MirageFileHandle;
typedef enum MirageStatus { MIRAGE_OK=0, MIRAGE_INVALID_ARGUMENT=1, MIRAGE_NOT_FOUND=2, MIRAGE_ACCESS_DENIED=3, MIRAGE_WOULD_BLOCK=4, MIRAGE_CANCELLED=5, MIRAGE_INTEGRITY_FAILURE=6, MIRAGE_BACKEND_UNAVAILABLE=7, MIRAGE_IO_ERROR=8, MIRAGE_CONFLICT=9, MIRAGE_DISK_FULL=10, MIRAGE_INTERNAL=255 } MirageStatus;
MirageStatus mirage_engine_create_empty(MirageEngineHandle **output);
MirageStatus mirage_engine_create_index(const uint16_t *path,size_t path_len,MirageEngineHandle **output);
MirageStatus mirage_engine_create_local(const uint16_t *index_path,size_t index_path_len,const uint16_t *object_root,size_t object_root_len,MirageEngineHandle **output);
MirageStatus mirage_engine_create_cache(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,MirageEngineHandle **output);
/* Same as mirage_engine_create_cache but non-resident pages fall back to the immutable origin pack directory; violation records carry outcome=origin|failed. */
MirageStatus mirage_engine_create_cache_with_origin(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,const uint16_t *origin_root,size_t origin_root_len,MirageEngineHandle **output);
/* The writable managed volume: durable namespace + journaled mutations; the cache shard is used when provisioned but is not required. `object_root` (nullable) is the local pack directory mirroring committed content. */
MirageStatus mirage_engine_create_managed(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,const uint16_t *object_root,size_t object_root_len,uint64_t dirty_budget_bytes,MirageEngineHandle **output);
/* Drive-capable managed engine: when drive_manifest_path (drive-manifest.cbor) and repository_key_path (repository-key.dpapi) are given, an on-demand Drive fetch provider is prepared and installed at the first mirage_engine_set_drive_token call. */
MirageStatus mirage_engine_create_managed_drive(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,const uint16_t *object_root,size_t object_root_len,uint64_t dirty_budget_bytes,const uint16_t *drive_manifest_path,size_t drive_manifest_len,const uint16_t *repository_key_path,size_t repository_key_len,MirageEngineHandle **output);
/* Supplies or rotates the Drive bearer token for a managed engine; the first call installs the page provider. Token bytes are never logged. */
MirageStatus mirage_engine_set_drive_token(MirageEngineHandle *engine,const uint8_t *token,size_t token_len);
/* Write-admission free-space floor for the journal volume (--floor; 0 disables). */
MirageStatus mirage_engine_set_disk_floor(MirageEngineHandle *engine,uint64_t floor_bytes);
/* EVICT <bytes> stdin command: evicts published payloads oldest-first. */
MirageStatus mirage_engine_evict_published(MirageEngineHandle *engine,uint64_t target_bytes,uint64_t *freed_bytes,uint64_t *blocked_bytes);
MirageStatus mirage_engine_reload_pins(MirageEngineHandle *engine);
/* Remaining dirty-payload budget for a managed volume; legacy engines report their configured free space is unavailable. */
MirageStatus mirage_engine_dirty_free(const MirageEngineHandle *engine,uint64_t *output);
/* Payload publication counters for a managed volume. */
typedef struct MiragePublicationStats {
  uint64_t pending_payloads;
  uint64_t pending_bytes;
  uint64_t published_payloads;
  uint64_t published_bytes;
  uint64_t evicted_payloads;
  uint64_t integrity_refusals;
  uint8_t last_error_class[32];
} MiragePublicationStats;
MirageStatus mirage_engine_publication_stats(const MirageEngineHandle *engine,MiragePublicationStats *output);
/* Quiesce-time compaction: drops superseded extent versions and reclaims
   dead journal payloads. Never fails a mount/unmount — callers log and
   continue on non-OK. No-op for engines without managed state. */
MirageStatus mirage_engine_compact(const MirageEngineHandle *engine);
MirageStatus mirage_engine_destroy(MirageEngineHandle *handle);
/* Ownership epoch of the volume coordinator; 0 for legacy read-only engines. */
MirageStatus mirage_engine_epoch(const MirageEngineHandle *engine,uint64_t *output);
/* Transition the owned volume to Mounted once the dispatcher is live. */
MirageStatus mirage_engine_mark_mounted(const MirageEngineHandle *engine);
/* Stop admitting reads, drain active readers up to timeout_ms, mark unmounted. */
MirageStatus mirage_engine_quiesce(const MirageEngineHandle *engine,uint32_t timeout_ms);
MirageStatus mirage_lookup(const MirageEngineHandle *engine,const uint16_t *path,size_t path_len,MirageFileHandle **output);
MirageStatus mirage_file_close(MirageFileHandle *handle);
typedef struct MirageFileInfo { uint64_t stable_index; uint64_t size; uint8_t directory; uint8_t reserved[7]; int64_t created_ns; int64_t modified_ns; } MirageFileInfo;
MirageStatus mirage_file_stat(const MirageFileHandle *handle,MirageFileInfo *output);
MirageStatus mirage_set_times(MirageFileHandle *handle,int64_t created_ns,int64_t modified_ns);
MirageStatus mirage_read(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred);
/* Same as mirage_read but records the calling process id in seal-violation records. */
MirageStatus mirage_read_ex(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred,uint32_t caller_pid);
/* Host read-ahead: same as mirage_read but a non-resident page is not recorded as a seal violation. */
MirageStatus mirage_read_speculative(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred);
/* Durable namespace mutations; each applies offline and journals the operation for later publication. */
MirageStatus mirage_namespace_create(MirageEngineHandle *engine,const uint16_t *path,size_t path_len,uint8_t directory);
MirageStatus mirage_namespace_rename(MirageEngineHandle *engine,const uint16_t *from_path,size_t from_len,const uint16_t *to_path,size_t to_len);
MirageStatus mirage_namespace_delete(MirageEngineHandle *engine,const uint16_t *path,size_t path_len);
/* Versioned byte-extent write path: payloads are journaled and fsynced before the extent version commits. */
MirageStatus mirage_write(MirageFileHandle *handle,uint64_t offset,const uint8_t *bytes,size_t bytes_len,size_t *transferred);
MirageStatus mirage_truncate(MirageFileHandle *handle,uint64_t new_size);
/* FlushFileBuffers: acknowledges local durability for committed operations; never a cloud signal. */
MirageStatus mirage_flush(MirageFileHandle *handle);
typedef uint8_t (*MirageEnumerateCallback)(void *context,const uint16_t *name,size_t name_len,MirageFileInfo info);
MirageStatus mirage_enumerate(const MirageFileHandle *handle,const uint16_t *marker,size_t marker_len,size_t limit,void *context,MirageEnumerateCallback callback);
#ifdef __cplusplus
}
#endif
#endif

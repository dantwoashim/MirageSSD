#ifndef MIRAGE_FFI_H
#define MIRAGE_FFI_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef struct MirageEngineHandle MirageEngineHandle;
typedef struct MirageFileHandle MirageFileHandle;
typedef enum MirageStatus { MIRAGE_OK=0, MIRAGE_INVALID_ARGUMENT=1, MIRAGE_NOT_FOUND=2, MIRAGE_ACCESS_DENIED=3, MIRAGE_WOULD_BLOCK=4, MIRAGE_CANCELLED=5, MIRAGE_INTEGRITY_FAILURE=6, MIRAGE_BACKEND_UNAVAILABLE=7, MIRAGE_IO_ERROR=8, MIRAGE_CONFLICT=9, MIRAGE_INTERNAL=255 } MirageStatus;
MirageStatus mirage_engine_create_empty(MirageEngineHandle **output);
MirageStatus mirage_engine_create_index(const uint16_t *path,size_t path_len,MirageEngineHandle **output);
MirageStatus mirage_engine_create_local(const uint16_t *index_path,size_t index_path_len,const uint16_t *object_root,size_t object_root_len,MirageEngineHandle **output);
MirageStatus mirage_engine_create_cache(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,MirageEngineHandle **output);
/* Same as mirage_engine_create_cache but non-resident pages fall back to the immutable origin pack directory; violation records carry outcome=origin|failed. */
MirageStatus mirage_engine_create_cache_with_origin(const uint16_t *index_path,size_t index_path_len,const uint16_t *state_root,size_t state_root_len,const uint16_t *origin_root,size_t origin_root_len,MirageEngineHandle **output);
MirageStatus mirage_engine_destroy(MirageEngineHandle *handle);
/* Ownership epoch of the volume coordinator; 0 for legacy read-only engines. */
MirageStatus mirage_engine_epoch(const MirageEngineHandle *engine,uint64_t *output);
/* Transition the owned volume to Mounted once the dispatcher is live. */
MirageStatus mirage_engine_mark_mounted(const MirageEngineHandle *engine);
/* Stop admitting reads, drain active readers up to timeout_ms, mark unmounted. */
MirageStatus mirage_engine_quiesce(const MirageEngineHandle *engine,uint32_t timeout_ms);
MirageStatus mirage_lookup(const MirageEngineHandle *engine,const uint16_t *path,size_t path_len,MirageFileHandle **output);
MirageStatus mirage_file_close(MirageFileHandle *handle);
typedef struct MirageFileInfo { uint64_t stable_index; uint64_t size; uint8_t directory; uint8_t reserved[7]; } MirageFileInfo;
MirageStatus mirage_file_stat(const MirageFileHandle *handle,MirageFileInfo *output);
MirageStatus mirage_read(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred);
/* Same as mirage_read but records the calling process id in seal-violation records. */
MirageStatus mirage_read_ex(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred,uint32_t caller_pid);
/* Host read-ahead: same as mirage_read but a non-resident page is not recorded as a seal violation. */
MirageStatus mirage_read_speculative(const MirageFileHandle *handle,uint64_t offset,uint8_t *output,size_t output_len,size_t *transferred);
/* Durable namespace mutations; each applies offline and journals the operation for later publication. */
MirageStatus mirage_namespace_create(MirageEngineHandle *engine,const uint16_t *path,size_t path_len,uint8_t directory);
MirageStatus mirage_namespace_rename(MirageEngineHandle *engine,const uint16_t *from_path,size_t from_len,const uint16_t *to_path,size_t to_len);
MirageStatus mirage_namespace_delete(MirageEngineHandle *engine,const uint16_t *path,size_t path_len);
typedef uint8_t (*MirageEnumerateCallback)(void *context,const uint16_t *name,size_t name_len,MirageFileInfo info);
MirageStatus mirage_enumerate(const MirageFileHandle *handle,const uint16_t *marker,size_t marker_len,size_t limit,void *context,MirageEnumerateCallback callback);
#ifdef __cplusplus
}
#endif
#endif

#pragma once
#include "mirage_ffi.h"
#include <cstdint>
namespace mirage::trace {
// One slot per attributed entry point: callback slots are named after the
// WinFsp interface member, FFI slots after the mirage_* export the host calls.
enum Slot : std::uint8_t {
    cb_GetVolumeInfo, cb_GetSecurityByName, cb_Create, cb_Open, cb_Overwrite,
    cb_Cleanup, cb_Close, cb_Read, cb_Write, cb_Flush, cb_GetFileInfo,
    cb_SetBasicInfo, cb_SetFileSize, cb_Rename, cb_ReadDirectory, cb_SetDelete,
    ffi_mirage_engine_compact, ffi_mirage_engine_create_cache,
    ffi_mirage_engine_create_cache_with_origin, ffi_mirage_engine_create_local,
    ffi_mirage_engine_create_managed_drive_at, ffi_mirage_engine_destroy,
    ffi_mirage_engine_dirty_free, ffi_mirage_engine_evict_published,
    ffi_mirage_engine_mark_mounted, ffi_mirage_engine_quiesce,
    ffi_mirage_engine_reload_pins, ffi_mirage_engine_set_disk_floor,
    ffi_mirage_engine_set_drive_token, ffi_mirage_enumerate,
    ffi_mirage_file_close, ffi_mirage_file_stat, ffi_mirage_flush,
    ffi_mirage_lookup, ffi_mirage_namespace_create,
    ffi_mirage_namespace_delete, ffi_mirage_namespace_rename,
    ffi_mirage_read_ex, ffi_mirage_set_times, ffi_mirage_truncate,
    ffi_mirage_write,
    slot_count
};
// True once MIRAGE_FS_TRACE named a TSV output path; the check is a single
// cached bool. record/dump are no-ops while tracing is off.
bool enabled() noexcept;
std::int64_t ticks() noexcept;
void record(Slot, std::uint64_t elapsed_ticks) noexcept;
void dump() noexcept;
// A host that is force-killed never reaches the orderly-stop dump, so while
// tracing is on the TSV is also refreshed periodically. start/stop are
// no-ops when MIRAGE_FS_TRACE is unset.
void start_flusher() noexcept;
void stop_flusher() noexcept;
// Times one attributed call. Construct at the top of a callback or FFI
// wrapper; when tracing is off the constructor is one bool check and the
// destructor a single branch.
class Scope {
public:
    explicit Scope(Slot slot) noexcept : slot_(slot) {
        if (enabled()) start_ = ticks();
    }
    ~Scope() {
        if (start_ < 0) return;
        record(slot_, static_cast<std::uint64_t>(ticks() - start_));
    }
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;
private:
    Slot slot_;
    std::int64_t start_{-1};
};
// Each wrapper keeps the mirage_* name and signature, scopes the call, and
// forwards; call sites write tr::mirage_* and see one slot per export.
#define MIRAGE_TRACED_FFI(name)                                          \
    template<typename... A>                                               \
    inline auto name(A&&... a) -> decltype(::name(static_cast<A&&>(a)...)) { \
        ::mirage::trace::Scope scope(::mirage::trace::ffi_##name);         \
        return ::name(static_cast<A&&>(a)...);                            \
    }
MIRAGE_TRACED_FFI(mirage_engine_compact)
MIRAGE_TRACED_FFI(mirage_engine_create_cache)
MIRAGE_TRACED_FFI(mirage_engine_create_cache_with_origin)
MIRAGE_TRACED_FFI(mirage_engine_create_local)
MIRAGE_TRACED_FFI(mirage_engine_create_managed_drive_at)
MIRAGE_TRACED_FFI(mirage_engine_destroy)
MIRAGE_TRACED_FFI(mirage_engine_dirty_free)
MIRAGE_TRACED_FFI(mirage_engine_evict_published)
MIRAGE_TRACED_FFI(mirage_engine_mark_mounted)
MIRAGE_TRACED_FFI(mirage_engine_quiesce)
MIRAGE_TRACED_FFI(mirage_engine_reload_pins)
MIRAGE_TRACED_FFI(mirage_engine_set_disk_floor)
MIRAGE_TRACED_FFI(mirage_engine_set_drive_token)
MIRAGE_TRACED_FFI(mirage_enumerate)
MIRAGE_TRACED_FFI(mirage_file_close)
MIRAGE_TRACED_FFI(mirage_file_stat)
MIRAGE_TRACED_FFI(mirage_flush)
MIRAGE_TRACED_FFI(mirage_lookup)
MIRAGE_TRACED_FFI(mirage_namespace_create)
MIRAGE_TRACED_FFI(mirage_namespace_delete)
MIRAGE_TRACED_FFI(mirage_namespace_rename)
MIRAGE_TRACED_FFI(mirage_read_ex)
MIRAGE_TRACED_FFI(mirage_set_times)
MIRAGE_TRACED_FFI(mirage_truncate)
MIRAGE_TRACED_FFI(mirage_write)
#undef MIRAGE_TRACED_FFI
}

#include "trace.hpp"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <thread>
#include <vector>
#include <windows.h>
namespace mirage::trace {
namespace {
struct Counter {
    std::atomic_uint64_t count{0};
    std::atomic_uint64_t total{0};
    std::atomic_uint64_t max{0};
};
struct SlotName {
    const char* kind;
    const char* name;
};
const SlotName slot_names[] = {
    {"callback", "GetVolumeInfo"}, {"callback", "GetSecurityByName"},
    {"callback", "Create"}, {"callback", "Open"}, {"callback", "Overwrite"},
    {"callback", "Cleanup"}, {"callback", "Close"}, {"callback", "Read"},
    {"callback", "Write"}, {"callback", "Flush"}, {"callback", "GetFileInfo"},
    {"callback", "SetBasicInfo"}, {"callback", "SetFileSize"},
    {"callback", "Rename"}, {"callback", "ReadDirectory"},
    {"callback", "SetDelete"},
    {"ffi", "mirage_engine_compact"}, {"ffi", "mirage_engine_create_cache"},
    {"ffi", "mirage_engine_create_cache_with_origin"},
    {"ffi", "mirage_engine_create_local"},
    {"ffi", "mirage_engine_create_managed_drive_at"},
    {"ffi", "mirage_engine_destroy"}, {"ffi", "mirage_engine_dirty_free"},
    {"ffi", "mirage_engine_evict_published"},
    {"ffi", "mirage_engine_mark_mounted"}, {"ffi", "mirage_engine_quiesce"},
    {"ffi", "mirage_engine_reload_pins"},
    {"ffi", "mirage_engine_set_disk_floor"},
    {"ffi", "mirage_engine_set_drive_token"}, {"ffi", "mirage_enumerate"},
    {"ffi", "mirage_file_close"}, {"ffi", "mirage_file_stat"},
    {"ffi", "mirage_flush"}, {"ffi", "mirage_lookup"},
    {"ffi", "mirage_namespace_create"}, {"ffi", "mirage_namespace_delete"},
    {"ffi", "mirage_namespace_rename"}, {"ffi", "mirage_read_ex"},
    {"ffi", "mirage_set_times"}, {"ffi", "mirage_truncate"},
    {"ffi", "mirage_write"},
};
static_assert(std::size(slot_names) == slot_count);
Counter counters[slot_count];
std::filesystem::path output_path;
std::int64_t frequency() noexcept {
    LARGE_INTEGER value{};
    QueryPerformanceFrequency(&value);
    return value.QuadPart ? value.QuadPart : 1;
}
bool read_trace_path() {
    wchar_t buffer[4096]{};
    const auto length = GetEnvironmentVariableW(L"MIRAGE_FS_TRACE", buffer, static_cast<DWORD>(std::size(buffer)));
    if (length == 0 || length >= std::size(buffer)) return false;
    output_path.assign(buffer, buffer + length);
    return true;
}
}
bool enabled() noexcept {
    static const bool on = read_trace_path();
    return on;
}
std::int64_t ticks() noexcept {
    LARGE_INTEGER now{};
    QueryPerformanceCounter(&now);
    return now.QuadPart;
}
void record(Slot slot, std::uint64_t elapsed_ticks) noexcept {
    auto& counter = counters[slot];
    counter.count.fetch_add(1, std::memory_order_relaxed);
    counter.total.fetch_add(elapsed_ticks, std::memory_order_relaxed);
    auto observed = counter.max.load(std::memory_order_relaxed);
    while (elapsed_ticks > observed &&
           !counter.max.compare_exchange_weak(observed, elapsed_ticks, std::memory_order_relaxed)) {
    }
}
void dump() noexcept {
    if (!enabled()) return;
    const auto frequency_value = frequency();
    struct Row {
        const SlotName* name;
        std::uint64_t count;
        std::uint64_t total_us;
        std::uint64_t max_us;
    };
    std::vector<Row> rows;
    rows.reserve(slot_count);
    for (std::size_t slot = 0; slot < slot_count; ++slot) {
        const auto count = counters[slot].count.load(std::memory_order_relaxed);
        if (count == 0) continue;
        rows.push_back({
            &slot_names[slot],
            count,
            counters[slot].total.load(std::memory_order_relaxed) * 1'000'000 / frequency_value,
            counters[slot].max.load(std::memory_order_relaxed) * 1'000'000 / frequency_value,
        });
    }
    std::sort(rows.begin(), rows.end(), [](const Row& a, const Row& b) { return a.total_us > b.total_us; });
    std::ofstream file(output_path, std::ios::out | std::ios::trunc);
    file << "kind\tname\tcount\ttotal_us\tmean_us\tmax_us\n";
    for (const auto& row : rows) {
        file << row.name->kind << '\t' << row.name->name << '\t' << row.count << '\t'
             << row.total_us << '\t' << (row.total_us + row.count / 2) / row.count << '\t'
             << row.max_us << '\n';
    }
}
namespace {
std::atomic_bool flusher_active{false};
std::thread flusher;
}
void start_flusher() noexcept {
    if (!enabled() || flusher_active.exchange(true)) return;
    flusher = std::thread([] {
        while (flusher_active.load(std::memory_order_relaxed)) {
            for (int i = 0; i < 10 && flusher_active.load(std::memory_order_relaxed); ++i) {
                std::this_thread::sleep_for(std::chrono::milliseconds(200));
            }
            dump();
        }
    });
}
void stop_flusher() noexcept {
    flusher_active.store(false);
    if (flusher.joinable()) flusher.join();
}
}

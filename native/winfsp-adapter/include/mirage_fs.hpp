#pragma once
#include "mirage_ffi.h"
#include <winfsp/winfsp.h>
#include <cstdint>
#include <string>
#include <atomic>
namespace mirage {
struct FileContext {
    std::atomic_uint32_t references{1};
    std::atomic_uint64_t last_end{};
    std::atomic_uint32_t sequential_reads{};
    std::atomic_bool prefetching{};
    MirageFileHandle* rust_handle{};
    MirageFileInfo info{};
};
class FileSystemHost {
public:
    ~FileSystemHost();
    FileSystemHost() = default;
    FileSystemHost(const FileSystemHost&) = delete;
    FileSystemHost& operator=(const FileSystemHost&) = delete;
    NTSTATUS mount(const std::wstring&, const std::wstring&, const std::wstring&, const std::wstring&, bool, std::uint64_t, std::uint64_t);
    NTSTATUS run();
    void stop() noexcept;
    MirageEngineHandle* engine() const noexcept { return engine_; }
    PSECURITY_DESCRIPTOR security() const noexcept { return security_descriptor_; }
    ULONG security_size() const noexcept { return security_size_; }
    ULONG async_delay_ms() const noexcept { return async_delay_ms_; }
    std::uint64_t volume_total_bytes() const noexcept { return volume_total_bytes_; }
    std::uint64_t volume_free_bytes() const noexcept { return volume_free_bytes_; }
    void begin_pending() noexcept;
    void end_pending() noexcept;
private:
    FSP_FILE_SYSTEM* fs_{};
    MirageEngineHandle* engine_{};
    PSECURITY_DESCRIPTOR security_descriptor_{};
    ULONG security_size_{};
    HANDLE stop_event_{};
    HANDLE pending_zero_{};
    std::atomic_uint32_t pending_count_{};
    ULONG async_delay_ms_{};
    std::uint64_t volume_total_bytes_{1};
    std::uint64_t volume_free_bytes_{};
};
void release_file_context(FileContext*) noexcept;
}
extern "C" NTSTATUS mirage_status_to_ntstatus(MirageStatus);

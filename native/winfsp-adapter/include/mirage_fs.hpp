#pragma once
#include "mirage_ffi.h"
#include <winfsp/winfsp.h>
#include <cstdint>
#include <string>
#include <atomic>
namespace mirage {
struct FileContext {
    std::atomic_uint32_t references{1};
    std::uint32_t opener_pid{};
    MirageFileHandle* rust_handle{};
    MirageFileInfo info{};
    // Set by SetDelete; the durable delete runs at cleanup when the flag and
    // FspCleanupDelete agree, matching Windows delete-pending semantics.
    bool delete_pending{};
};
class FileSystemHost {
public:
    ~FileSystemHost();
    FileSystemHost() = default;
    FileSystemHost(const FileSystemHost&) = delete;
    FileSystemHost& operator=(const FileSystemHost&) = delete;
    NTSTATUS mount(const std::wstring&, const std::wstring&, const std::wstring&, const std::wstring&, bool, bool, std::uint64_t, std::uint64_t, const std::wstring& = {}, const std::wstring& = {}, const std::wstring& = {}, std::uint64_t = 0);
    // Delivers a Drive bearer token to a managed engine; a no-op for
    // engines without a provider.
    void set_drive_token(const std::string& token);
    void request_eviction(const std::string& bytes);
    NTSTATUS run();
    void stop() noexcept;
    bool writable() const noexcept { return writable_; }
    MirageEngineHandle* engine() const noexcept { return engine_; }
    PSECURITY_DESCRIPTOR security() const noexcept { return security_descriptor_; }
    ULONG security_size() const noexcept { return security_size_; }
    ULONG async_delay_ms() const noexcept { return async_delay_ms_; }
    std::uint64_t volume_total_bytes() const noexcept { return volume_total_bytes_; }
    void set_volume_label(const std::wstring& label) { label_ = label.substr(0, 32); }
    const std::wstring& volume_label() const noexcept { return label_; }
    std::uint64_t volume_free_bytes() const noexcept { return volume_free_bytes_; }
    // Managed volumes with a separate local staging budget advertise the
    // remote (Drive) capacity to Windows and keep the budget internal.
    void set_dirty_budget(std::uint64_t bytes) noexcept { dirty_budget_ = bytes; }
    bool advertises_remote_capacity() const noexcept { return dirty_budget_ != 0; }
    // Journal payload directory on the user's chosen cache disk (--journal-root);
    // empty keeps the engine default <state-root>\journal.
    void set_journal_root(const std::wstring& root) { journal_root_ = root; }
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
    // Managed-volume mode: writable mutations and finite metadata caching.
    // Legacy cache/local mounts stay read-only with immutable-content caching.
    bool writable_{};
    std::uint64_t volume_total_bytes_{1};
    std::uint64_t volume_free_bytes_{};
    std::uint64_t dirty_budget_{};
    std::wstring journal_root_{};
    std::wstring label_{L"MirageSSD"};
};
void release_file_context(FileContext*) noexcept;
}
extern "C" NTSTATUS mirage_status_to_ntstatus(MirageStatus);

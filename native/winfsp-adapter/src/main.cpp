#include "mirage_fs.hpp"
#include <cerrno>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <string_view>
#include <thread>
namespace {
mirage::FileSystemHost* active{};
BOOL WINAPI stop_handler(DWORD) { if (active) active->stop(); return TRUE; }
bool parse_capacity(const wchar_t* text, std::uint64_t& value) {
    if (!text || !*text) return false;
    for (const wchar_t* current = text; *current; ++current) if (*current < L'0' || *current > L'9') return false;
    errno = 0; wchar_t* end{}; const auto parsed = std::wcstoull(text, &end, 10);
    if (errno == ERANGE || !end || *end != L'\0') return false;
    value = parsed; return true;
}
}
int wmain(int argc, wchar_t** argv) {
    if (argc != 5 && argc != 8 && argc != 10 && argc != 12 && argc != 14 && argc != 16) { std::wcerr << L"usage: mirage-fs <mount-directory> <mount-index> <storage-root> <owner-sid> [--cache|--managed total-bytes free-bytes [--origin path] [--drive-manifest p --repository-key p]]\n  --managed: total-bytes = advertised volume size; free-bytes = dirty-payload budget (writes beyond it fail with disk full)\n"; return 2; }
    const bool cache_mode = argc >= 8 && std::wstring_view(argv[5]) == L"--cache";
    const bool managed_mode = argc >= 8 && std::wstring_view(argv[5]) == L"--managed";
    if (argc >= 8 && !cache_mode && !managed_mode) { std::wcerr << L"unknown storage mode\n"; return 2; }
    // Trailing option pairs: --origin names the committed-content source (an
    // origin pack directory for --cache, the local pack mirror for --managed);
    // --drive-manifest + --repository-key enable the on-demand Drive provider
    // (managed only; the bearer token arrives on stdin as a TOKEN line).
    std::wstring origin, drive_manifest, repository_key, label;
    std::uint64_t disk_floor = 0;
    for (int i = 8; i < argc; i += 2) {
        const std::wstring_view flag = argv[i];
        if (i + 1 >= argc) { std::wcerr << L"missing value for " << flag << L"\n"; return 2; }
        if (flag == L"--origin") origin = argv[i + 1];
        else if (flag == L"--drive-manifest") drive_manifest = argv[i + 1];
        else if (flag == L"--repository-key") repository_key = argv[i + 1];
        else if (flag == L"--floor") { if (!parse_capacity(argv[i + 1], disk_floor)) { std::wcerr << L"invalid --floor\n"; return 2; } }
        else if (flag == L"--label") label = argv[i + 1];
        else { std::wcerr << L"unknown option " << flag << L"\n"; return 2; }
    }
    if (drive_manifest.empty() != repository_key.empty()) { std::wcerr << L"--drive-manifest and --repository-key must be supplied together\n"; return 2; }
    if (!drive_manifest.empty()) {
        if (!managed_mode) { std::wcerr << L"drive provider paths require --managed\n"; return 2; }
        if (!std::filesystem::exists(drive_manifest) || !std::filesystem::exists(repository_key)) { std::wcerr << L"drive manifest or repository key not found\n"; return 2; }
    }
    std::uint64_t total_bytes{}; std::uint64_t free_bytes{};
    if (cache_mode || managed_mode) {
        if (!parse_capacity(argv[6], total_bytes) || !parse_capacity(argv[7], free_bytes) || total_bytes == 0 || free_bytes > total_bytes) { std::wcerr << L"invalid volume capacity\n"; return 2; }
    } else {
        std::error_code error; const auto space = std::filesystem::space(argv[3], error);
        if (error || space.capacity == 0) { std::wcerr << L"storage capacity unavailable\n"; return 2; }
        total_bytes = space.capacity; free_bytes = space.available;
    }
    const std::filesystem::path mount = argv[1];
    const bool drive_mount = mount.native().size() == 2 && mount.native()[1] == L':';
    if (!drive_mount && std::filesystem::exists(mount) && (!std::filesystem::is_directory(mount) || !std::filesystem::is_empty(mount))) { std::wcerr << L"mount path must be a free drive letter, absent path, or empty directory\n"; return 2; }
    mirage::FileSystemHost host; active = &host; SetConsoleCtrlHandler(stop_handler, TRUE);
    // A managed mount is the local-first writable volume: same state root
    // and cache constructor as --cache, but mutation callbacks are live.
    if (!label.empty()) host.set_volume_label(label);
    const auto status = host.mount(mount.wstring(), argv[2], argv[3], argv[4], cache_mode || managed_mode, managed_mode, total_bytes, free_bytes, origin, drive_manifest, repository_key, disk_floor);
    if (!NT_SUCCESS(status)) { std::wcerr << L"mount failed status=0x" << std::hex << static_cast<unsigned long>(status) << L"\n"; return 1; }
    std::thread control;
    if (GetFileType(GetStdHandle(STD_INPUT_HANDLE)) == FILE_TYPE_PIPE) {
        control = std::thread([&host] {
            std::string command;
            while (std::getline(std::cin, command)) {
                if (command == "STOP") { host.stop(); return; }
                // TOKEN <bearer>: deliver to the provider; the token value is
                // never echoed or logged.
                if (command.compare(0, 6, "TOKEN ") == 0) { host.set_drive_token(command.substr(6)); continue; }
                // EVICT <bytes>: evict published payloads; reply MIRAGE_EVICTED <freed>.
                if (command.compare(0, 6, "EVICT ") == 0) { host.request_eviction(command.substr(6)); continue; }
                // PINS-RELOAD: refresh the pinned-inode set after pin/unpin.
                if (command == "PINS-RELOAD") { mirage_engine_reload_pins(host.engine()); continue; }
                if (!command.empty()) std::cerr << "ignoring unknown control command\n";
            }
        });
    }
    const auto result = host.run(); active = nullptr;
    if (control.joinable()) control.join();
    std::wcerr << L"dispatcher stopped status=0x" << std::hex << static_cast<unsigned long>(result) << L"\n";
    return result == STATUS_SUCCESS ? 0 : 1;
}

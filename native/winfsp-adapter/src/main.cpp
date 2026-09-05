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
    if (argc != 5 && argc != 8) { std::wcerr << L"usage: mirage-fs <mount-directory> <mount-index> <storage-root> <owner-sid> [--cache total-bytes free-bytes]\n"; return 2; }
    const bool cache_mode = argc == 8 && std::wstring_view(argv[5]) == L"--cache";
    if (argc == 8 && !cache_mode) { std::wcerr << L"unknown storage mode\n"; return 2; }
    std::uint64_t total_bytes{}; std::uint64_t free_bytes{};
    if (cache_mode) {
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
    const auto status = host.mount(mount.wstring(), argv[2], argv[3], argv[4], cache_mode, total_bytes, free_bytes);
    if (!NT_SUCCESS(status)) { std::wcerr << L"mount failed status=0x" << std::hex << static_cast<unsigned long>(status) << L"\n"; return 1; }
    std::thread control;
    if (GetFileType(GetStdHandle(STD_INPUT_HANDLE)) == FILE_TYPE_PIPE) {
        control = std::thread([&host] { std::string command; if (std::getline(std::cin, command) && command == "STOP") host.stop(); });
    }
    const auto result = host.run(); active = nullptr;
    if (control.joinable()) control.join();
    std::wcerr << L"dispatcher stopped status=0x" << std::hex << static_cast<unsigned long>(result) << L"\n";
    return result == STATUS_SUCCESS ? 0 : 1;
}

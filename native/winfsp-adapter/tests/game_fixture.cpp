// Synthetic launcher handoff fixture. No publisher executable or game assets.
#include <windows.h>
#include <fstream>
#include <filesystem>
#include <string>
int wmain(int argc, wchar_t**) {
    if (argc > 1) {
        Sleep(600); // Controller must identify the detached game first.
        std::ifstream input(std::filesystem::path(L"GenshinImpact_Data/StreamingAssets/AudioAssets/fixture.bnk"), std::ios::binary);
        std::string bytes((std::istreambuf_iterator<char>(input)), std::istreambuf_iterator<char>());
        if (bytes != "verified synthetic publisher fixture") return 9;
        std::ofstream("fixture-read-ok.txt") << "verified";
        Sleep(1000);
        return 0;
    }
    wchar_t executable[32768]{};
    if (!GetModuleFileNameW(nullptr, executable, 32768)) return 2;
    std::wstring command = L"\"" + std::wstring(executable) + L"\" --child";
    STARTUPINFOW startup{}; startup.cb = sizeof(startup);
    PROCESS_INFORMATION child{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW, nullptr, nullptr, &startup, &child)) return 3;
    CloseHandle(child.hThread); CloseHandle(child.hProcess);
    // Launcher deliberately exits while its detached child remains active.
    return 0;
}

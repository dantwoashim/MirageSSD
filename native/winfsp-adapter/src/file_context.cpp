#include <cstdint>
namespace mirage { struct FileContext { void* rust_handle{}; std::uint64_t stable_index{}; bool directory{}; }; }

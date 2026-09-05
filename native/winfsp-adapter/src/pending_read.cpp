#include <atomic>
namespace mirage { struct PendingRead { std::atomic<unsigned char> state{0}; }; }

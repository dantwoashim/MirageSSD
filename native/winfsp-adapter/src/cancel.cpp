#include <atomic>
namespace mirage { bool cancel_once(std::atomic<unsigned char>& state){unsigned char expected=0;return state.compare_exchange_strong(expected,2);} }

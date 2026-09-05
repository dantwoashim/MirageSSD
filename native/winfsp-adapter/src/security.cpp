#include <cstdint>
namespace mirage { std::uint32_t volume_serial(const std::uint8_t* id){std::uint32_t value=0x4d495247;for(int i=0;i<16;++i)value=(value<<5)^value^id[i];return value;} }

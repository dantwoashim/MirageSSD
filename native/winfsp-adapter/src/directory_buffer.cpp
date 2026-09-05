#include <winfsp/winfsp.h>
namespace mirage { bool finish_directory(PVOID buffer,ULONG length,PULONG transferred){return 0!=FspFileSystemAddDirInfo(nullptr,buffer,length,transferred);} }

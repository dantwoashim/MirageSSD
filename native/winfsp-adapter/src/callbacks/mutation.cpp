#include <winfsp/winfsp.h>
extern "C" NTSTATUS mirage_read_only_mutation_status(){return STATUS_MEDIA_WRITE_PROTECTED;}

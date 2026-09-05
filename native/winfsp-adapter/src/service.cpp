#include "mirage_fs.hpp"
#include <algorithm>
#include <cstring>
#include <new>
#include <sddl.h>
#include <vector>
#include <iostream>
#include <thread>
namespace {
using mirage::FileContext; using mirage::FileSystemHost;
FileSystemHost* host(FSP_FILE_SYSTEM* fs) { return static_cast<FileSystemHost*>(fs->UserContext); }
bool create_mount_security(const std::wstring& owner_sid, PSECURITY_DESCRIPTOR* descriptor, ULONG* size) {
    PSID parsed_sid{};
    if (!ConvertStringSidToSidW(owner_sid.c_str(), &parsed_sid) || !IsValidSid(parsed_sid)) return false;
    PWSTR sid{};
    if (!ConvertSidToStringSidW(parsed_sid, &sid)) { LocalFree(parsed_sid); return false; }
    std::wstring sddl = L"O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;";
    sddl += sid; sddl += L")"; LocalFree(sid); LocalFree(parsed_sid);
    return ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, descriptor, size) != FALSE;
}
void fill_info(const MirageFileInfo& source, FSP_FSCTL_FILE_INFO* output) {
    std::memset(output, 0, sizeof(*output));
    output->FileAttributes = source.directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_READONLY;
    output->AllocationSize = (source.size + 4095) & ~UINT64_C(4095); output->FileSize = source.size; output->IndexNumber = source.stable_index;
}
NTSTATUS lookup(FSP_FILE_SYSTEM* fs, PCWSTR name, MirageFileHandle** output, MirageFileInfo* info) {
    const auto status = mirage_lookup(host(fs)->engine(), reinterpret_cast<const uint16_t*>(name), std::wcslen(name), output);
    if (status != MIRAGE_OK) return mirage_status_to_ntstatus(status);
    const auto stat = mirage_file_stat(*output, info);
    if (stat != MIRAGE_OK) { mirage_file_close(*output); *output = nullptr; }
    return mirage_status_to_ntstatus(stat);
}
NTSTATUS get_volume(FSP_FILE_SYSTEM* fs, FSP_FSCTL_VOLUME_INFO* info) {
    std::memset(info, 0, sizeof(*info)); info->TotalSize = host(fs)->volume_total_bytes(); info->FreeSize = host(fs)->volume_free_bytes();
    constexpr wchar_t label[] = L"MirageSSD"; info->VolumeLabelLength = static_cast<UINT16>((std::size(label)-1)*sizeof(wchar_t)); std::copy_n(label, std::size(label), info->VolumeLabel); return STATUS_SUCCESS;
}
NTSTATUS security_by_name(FSP_FILE_SYSTEM* fs, PWSTR name, PUINT32 attributes, PSECURITY_DESCRIPTOR output, SIZE_T* size) {
    MirageFileHandle* file{}; MirageFileInfo info{}; const auto status = lookup(fs, name, &file, &info); if (!NT_SUCCESS(status)) return status; mirage_file_close(file);
    if (attributes) *attributes = info.directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_READONLY;
    if (size) { const auto required = static_cast<SIZE_T>(host(fs)->security_size()); if (required > *size) { *size = required; return STATUS_BUFFER_OVERFLOW; } *size = required; if (output) std::memcpy(output, host(fs)->security(), required); }
    return STATUS_SUCCESS;
}
NTSTATUS create(FSP_FILE_SYSTEM*, PWSTR, UINT32, UINT32, UINT32, PSECURITY_DESCRIPTOR, UINT64, PVOID*, FSP_FSCTL_FILE_INFO*) { return STATUS_MEDIA_WRITE_PROTECTED; }
NTSTATUS open_file(FSP_FILE_SYSTEM* fs, PWSTR name, UINT32, UINT32, PVOID* context, FSP_FSCTL_FILE_INFO* info) {
    MirageFileHandle* file{}; MirageFileInfo stat{}; const auto status = lookup(fs, name, &file, &stat); if (!NT_SUCCESS(status)) return status;
    auto* opened = new (std::nothrow) FileContext{}; if (!opened) { mirage_file_close(file); return STATUS_INSUFFICIENT_RESOURCES; } opened->rust_handle=file; opened->info=stat;
    *context = opened; fill_info(stat, info); return STATUS_SUCCESS;
}
void close_file(FSP_FILE_SYSTEM*, PVOID context) { mirage::release_file_context(static_cast<FileContext*>(context)); }
NTSTATUS get_info(FSP_FILE_SYSTEM*, PVOID context, FSP_FSCTL_FILE_INFO* info) { auto* opened = static_cast<FileContext*>(context); if (!opened) return STATUS_INVALID_HANDLE; fill_info(opened->info, info); return STATUS_SUCCESS; }
struct Child { std::wstring name; MirageFileInfo info; };
uint8_t collect_child(void* context, const uint16_t* name, size_t length, MirageFileInfo info) { static_cast<std::vector<Child>*>(context)->push_back({std::wstring(reinterpret_cast<const wchar_t*>(name), length), info}); return 1; }
NTSTATUS read_dir(FSP_FILE_SYSTEM*, PVOID context, PWSTR, PWSTR marker, PVOID buffer, ULONG length, PULONG transferred) {
    auto* opened = static_cast<FileContext*>(context); if (!opened || !opened->info.directory) return STATUS_NOT_A_DIRECTORY;
    std::vector<Child> children; const size_t marker_length = marker ? std::wcslen(marker) : 0;
    const auto status = mirage_enumerate(opened->rust_handle, reinterpret_cast<const uint16_t*>(marker), marker_length, 4096, &children, collect_child); if (status != MIRAGE_OK) return mirage_status_to_ntstatus(status);
    *transferred = 0;
    for (const auto& child : children) { const auto name_bytes = child.name.size()*sizeof(wchar_t); std::vector<unsigned char> storage(sizeof(FSP_FSCTL_DIR_INFO)+name_bytes); auto* entry = reinterpret_cast<FSP_FSCTL_DIR_INFO*>(storage.data()); std::memset(entry,0,storage.size()); entry->Size=static_cast<UINT16>(sizeof(FSP_FSCTL_DIR_INFO)+name_bytes); fill_info(child.info,&entry->FileInfo); std::memcpy(entry->FileNameBuf,child.name.data(),name_bytes); if (!FspFileSystemAddDirInfo(entry,buffer,length,transferred)) break; }
    FspFileSystemAddDirInfo(nullptr,buffer,length,transferred); return STATUS_SUCCESS;
}
void record_read(FileSystemHost* owner,FileContext* opened,UINT64 offset,size_t read){
    const auto end=offset+read;const auto previous=opened->last_end.exchange(end);const auto sequence=previous==offset?opened->sequential_reads.fetch_add(1)+1:0;if(previous!=offset)opened->sequential_reads.store(0);
    if(sequence<2||end>=opened->info.size||opened->prefetching.exchange(true))return;
    opened->references.fetch_add(1);owner->begin_pending();
    try{std::thread([owner,opened,end]{const auto remaining=opened->info.size-end;const auto length=static_cast<size_t>(std::min<UINT64>(remaining,64*1024));std::vector<uint8_t> bytes(length);size_t transferred{};mirage_read(opened->rust_handle,end,bytes.data(),bytes.size(),&transferred);opened->prefetching.store(false);mirage::release_file_context(opened);owner->end_pending();}).detach();}
    catch(...){opened->prefetching.store(false);mirage::release_file_context(opened);owner->end_pending();}
}
NTSTATUS read_file(FSP_FILE_SYSTEM* fs, PVOID context, PVOID buffer, UINT64 offset, ULONG length, PULONG transferred) {
    auto* opened=static_cast<FileContext*>(context); if(!opened||opened->info.directory)return STATUS_FILE_IS_A_DIRECTORY;
    auto* owner=host(fs);if(owner->async_delay_ms()!=0){
        const auto hint=FspFileSystemGetOperationContext()->Request->Hint;opened->references.fetch_add(1);owner->begin_pending();
        try{std::thread([fs,owner,opened,buffer,offset,length,hint]{Sleep(owner->async_delay_ms());size_t read{};const auto result=mirage_read(opened->rust_handle,offset,static_cast<uint8_t*>(buffer),length,&read);if(result==MIRAGE_OK)record_read(owner,opened,offset,read);FSP_FSCTL_TRANSACT_RSP response;std::memset(&response,0,sizeof(response));response.Size=sizeof(response);response.Kind=FspFsctlTransactReadKind;response.Hint=hint;response.IoStatus.Status=mirage_status_to_ntstatus(result);response.IoStatus.Information=static_cast<UINT32>(read);FspFileSystemSendResponse(fs,&response);mirage::release_file_context(opened);owner->end_pending();}).detach();return STATUS_PENDING;}catch(...){mirage::release_file_context(opened);owner->end_pending();}
    }
    size_t read{};const auto status=mirage_read(opened->rust_handle,offset,static_cast<uint8_t*>(buffer),length,&read);if(status==MIRAGE_OK)record_read(owner,opened,offset,read);*transferred=static_cast<ULONG>(read);return mirage_status_to_ntstatus(status);
}
NTSTATUS overwrite(FSP_FILE_SYSTEM*,PVOID,UINT32,BOOLEAN,UINT64,FSP_FSCTL_FILE_INFO*){return STATUS_MEDIA_WRITE_PROTECTED;}
FSP_FILE_SYSTEM_INTERFACE interface_table={.GetVolumeInfo=get_volume,.GetSecurityByName=security_by_name,.Create=create,.Open=open_file,.Overwrite=overwrite,.Close=close_file,.Read=read_file,.GetFileInfo=get_info,.ReadDirectory=read_dir};
}
namespace mirage {
void release_file_context(FileContext* context) noexcept{if(context&&context->references.fetch_sub(1)==1){mirage_file_close(context->rust_handle);delete context;}}
FileSystemHost::~FileSystemHost(){stop();if(stop_event_)CloseHandle(stop_event_);if(pending_zero_)CloseHandle(pending_zero_);if(engine_)mirage_engine_destroy(engine_);if(security_descriptor_)LocalFree(security_descriptor_);}
NTSTATUS FileSystemHost::mount(const std::wstring& path,const std::wstring& index_path,const std::wstring& storage_root,const std::wstring& owner_sid,bool cache_mode,std::uint64_t volume_total_bytes,std::uint64_t volume_free_bytes){
    if(volume_total_bytes==0||volume_free_bytes>volume_total_bytes)return STATUS_INVALID_PARAMETER;volume_total_bytes_=volume_total_bytes;volume_free_bytes_=volume_free_bytes;
    stop_event_=CreateEventW(nullptr,TRUE,FALSE,nullptr);pending_zero_=CreateEventW(nullptr,TRUE,TRUE,nullptr);if(!stop_event_||!pending_zero_)return FspNtStatusFromWin32(GetLastError());wchar_t delay[32]{};if(GetEnvironmentVariableW(L"MIRAGE_TEST_ASYNC_DELAY_MS",delay,static_cast<DWORD>(std::size(delay))))async_delay_ms_=wcstoul(delay,nullptr,10);auto ffi=cache_mode?mirage_engine_create_cache(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),&engine_):mirage_engine_create_local(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),&engine_);if(ffi!=MIRAGE_OK)return mirage_status_to_ntstatus(ffi);
    if(!create_mount_security(owner_sid,&security_descriptor_,&security_size_))return FspNtStatusFromWin32(GetLastError());
    FSP_FSCTL_VOLUME_PARAMS params{};params.SectorSize=4096;params.SectorsPerAllocationUnit=1;params.FileInfoTimeout=1000;params.CasePreservedNames=1;params.UnicodeOnDisk=1;params.PersistentAcls=1;params.ReadOnlyVolume=1;wcscpy_s(params.FileSystemName,L"MirageSSD");
    auto status=FspFileSystemCreate(const_cast<PWSTR>(L"" FSP_FSCTL_DISK_DEVICE_NAME),&params,&interface_table,&fs_);if(!NT_SUCCESS(status))return status;fs_->UserContext=this;status=FspFileSystemSetMountPoint(fs_,const_cast<PWSTR>(path.c_str()));if(!NT_SUCCESS(status)){FspFileSystemDelete(fs_);fs_=nullptr;}return status;
}
NTSTATUS FileSystemHost::run(){if(!fs_)return STATUS_INVALID_DEVICE_STATE;const auto status=FspFileSystemStartDispatcher(fs_,0);if(!NT_SUCCESS(status))return status;std::cout<<"MIRAGE_READY\n"<<std::flush;WaitForSingleObject(stop_event_,INFINITE);return STATUS_SUCCESS;}
void FileSystemHost::begin_pending() noexcept{if(pending_count_.fetch_add(1)==0)ResetEvent(pending_zero_);}
void FileSystemHost::end_pending() noexcept{if(pending_count_.fetch_sub(1)==1)SetEvent(pending_zero_);}
void FileSystemHost::stop() noexcept{if(fs_){WaitForSingleObject(pending_zero_,INFINITE);FspFileSystemStopDispatcher(fs_);FspFileSystemDelete(fs_);fs_=nullptr;}if(stop_event_)SetEvent(stop_event_);}
}

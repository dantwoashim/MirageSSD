#include "mirage_fs.hpp"
#include <algorithm>
#include <cstdlib>
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
static UINT64 process_start_filetime() {
    // Fallback stamp for files whose namespace timestamps are absent.
    static const UINT64 value = [] {
        FILETIME created{}, dummy1{}, dummy2{}, dummy3{};
        if (GetProcessTimes(GetCurrentProcess(), &created, &dummy1, &dummy2, &dummy3)) {
            return (UINT64(created.dwHighDateTime) << 32) | created.dwLowDateTime;
        }
        FILETIME now{}; GetSystemTimeAsFileTime(&now);
        return (UINT64(now.dwHighDateTime) << 32) | now.dwLowDateTime;
    }();
    return value;
}
static UINT64 ns_to_filetime(int64_t ns) {
    if (ns <= 0) return process_start_filetime();
    return UINT64(ns / 100) + 116444736000000000ULL;
}
void fill_info(const MirageFileInfo& source, FSP_FSCTL_FILE_INFO* output, bool writable) {
    std::memset(output, 0, sizeof(*output));
    // A managed writable volume must not advertise read-only files; legacy
    // mounts keep the immutable-content attribute.
    output->FileAttributes = source.directory ? FILE_ATTRIBUTE_DIRECTORY
        : (writable ? FILE_ATTRIBUTE_NORMAL : FILE_ATTRIBUTE_READONLY);
    output->AllocationSize = (source.size + 4095) & ~UINT64_C(4095); output->FileSize = source.size; output->IndexNumber = source.stable_index;
    output->CreationTime = ns_to_filetime(source.created_ns);
    output->LastWriteTime = ns_to_filetime(source.modified_ns);
    output->ChangeTime = output->LastWriteTime;
    output->LastAccessTime = output->LastWriteTime;
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
    // Managed volumes advertise the live dirty-payload budget as free space.
    if(host(fs)->writable()&&host(fs)->engine()&&!host(fs)->advertises_remote_capacity()){std::uint64_t free_bytes=info->FreeSize;if(mirage_engine_dirty_free(host(fs)->engine(),&free_bytes)==MIRAGE_OK)info->FreeSize=free_bytes;}
    const auto& label = host(fs)->volume_label(); info->VolumeLabelLength = static_cast<UINT16>(label.size()*sizeof(wchar_t)); std::copy_n(label.data(), label.size(), info->VolumeLabel); return STATUS_SUCCESS;
}
NTSTATUS security_by_name(FSP_FILE_SYSTEM* fs, PWSTR name, PUINT32 attributes, PSECURITY_DESCRIPTOR output, SIZE_T* size) {
    MirageFileHandle* file{}; MirageFileInfo info{}; const auto status = lookup(fs, name, &file, &info); if (!NT_SUCCESS(status)) return status; mirage_file_close(file);
    if (attributes) *attributes = info.directory ? FILE_ATTRIBUTE_DIRECTORY : (host(fs)->writable() ? FILE_ATTRIBUTE_NORMAL : FILE_ATTRIBUTE_READONLY);
    if (size) { const auto required = static_cast<SIZE_T>(host(fs)->security_size()); if (required > *size) { *size = required; return STATUS_BUFFER_OVERFLOW; } *size = required; if (output) std::memcpy(output, host(fs)->security(), required); }
    return STATUS_SUCCESS;
}
// WinFsp encodes the create disposition in the upper byte of CreateOptions.
NTSTATUS create(FSP_FILE_SYSTEM* fs, PWSTR name, UINT32 create_options, UINT32, UINT32, PSECURITY_DESCRIPTOR, UINT64, PVOID* context, FSP_FSCTL_FILE_INFO* info) {
    const uint8_t directory=(create_options&FILE_DIRECTORY_FILE)!=0?1:0;
    const UINT32 disposition=create_options>>24;
    if(!host(fs)->writable()&&(disposition==FILE_CREATE||disposition==FILE_OVERWRITE_IF||disposition==FILE_OPEN_IF||disposition==FILE_SUPERSEDE))
        return STATUS_MEDIA_WRITE_PROTECTED;
    if(disposition==FILE_CREATE||disposition==FILE_OVERWRITE_IF||disposition==FILE_OPEN_IF||disposition==FILE_SUPERSEDE){
        const auto status=mirage_namespace_create(host(fs)->engine(),reinterpret_cast<const uint16_t*>(name),std::wcslen(name),directory);
        // OPEN_IF/SUPERSEDE may still resolve an existing entry.
        if(status!=MIRAGE_OK&&!(disposition!=FILE_CREATE&&status==MIRAGE_CONFLICT))return mirage_status_to_ntstatus(status);
    }
    MirageFileHandle* file{}; MirageFileInfo stat{}; const auto status=lookup(fs,name,&file,&stat); if(!NT_SUCCESS(status)) return status;
    auto* opened=new (std::nothrow) FileContext{}; if(!opened){mirage_file_close(file);return STATUS_INSUFFICIENT_RESOURCES;} opened->rust_handle=file; opened->info=stat; opened->opener_pid=FspFileSystemOperationProcessId();
    *context=opened; fill_info(stat,info,host(fs)->writable()); return STATUS_SUCCESS;
}
NTSTATUS rename_file(FSP_FILE_SYSTEM* fs, PVOID context, PWSTR name, PWSTR new_name, BOOLEAN) {
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    if(!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    return mirage_status_to_ntstatus(mirage_namespace_rename(host(fs)->engine(),reinterpret_cast<const uint16_t*>(name),std::wcslen(name),reinterpret_cast<const uint16_t*>(new_name),std::wcslen(new_name)));
}
NTSTATUS set_delete(FSP_FILE_SYSTEM* fs, PVOID context, PWSTR, BOOLEAN delete_file) {
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    if(delete_file&&!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    opened->delete_pending=delete_file!=FALSE;
    return STATUS_SUCCESS;
}
void cleanup(FSP_FILE_SYSTEM* fs, PVOID context, PWSTR name, ULONG flags) {
    auto* opened=static_cast<FileContext*>(context); if(!opened) return;
    // FspCleanupDelete is sent on the last close of a delete-pending file —
    // either via SetDelete or a delete-on-close open (which never calls
    // SetDelete). The durable delete is issued here so open readers keep
    // identity until then.
    if((flags&FspCleanupDelete)&&host(fs)->writable()){
        mirage_namespace_delete(host(fs)->engine(),reinterpret_cast<const uint16_t*>(name),std::wcslen(name));
    }
}
NTSTATUS open_file(FSP_FILE_SYSTEM* fs, PWSTR name, UINT32, UINT32, PVOID* context, FSP_FSCTL_FILE_INFO* info) {
    MirageFileHandle* file{}; MirageFileInfo stat{}; const auto status = lookup(fs, name, &file, &stat); if (!NT_SUCCESS(status)) return status;
    auto* opened = new (std::nothrow) FileContext{}; if (!opened) { mirage_file_close(file); return STATUS_INSUFFICIENT_RESOURCES; } opened->rust_handle=file; opened->info=stat; opened->opener_pid=FspFileSystemOperationProcessId();
    *context = opened; fill_info(stat, info, host(fs)->writable()); return STATUS_SUCCESS;
}
void close_file(FSP_FILE_SYSTEM*, PVOID context) { mirage::release_file_context(static_cast<FileContext*>(context)); }
NTSTATUS get_info(FSP_FILE_SYSTEM* fs, PVOID context, FSP_FSCTL_FILE_INFO* info) { auto* opened = static_cast<FileContext*>(context); if (!opened) return STATUS_INVALID_HANDLE; fill_info(opened->info, info, host(fs)->writable()); return STATUS_SUCCESS; }
struct Child { std::wstring name; MirageFileInfo info; };
uint8_t collect_child(void* context, const uint16_t* name, size_t length, MirageFileInfo info) { static_cast<std::vector<Child>*>(context)->push_back({std::wstring(reinterpret_cast<const wchar_t*>(name), length), info}); return 1; }
NTSTATUS read_dir(FSP_FILE_SYSTEM* fs, PVOID context, PWSTR, PWSTR marker, PVOID buffer, ULONG length, PULONG transferred) {
    auto* opened = static_cast<FileContext*>(context); if (!opened || !opened->info.directory) return STATUS_NOT_A_DIRECTORY;
    std::vector<Child> children; const size_t marker_length = marker ? std::wcslen(marker) : 0;
    const auto status = mirage_enumerate(opened->rust_handle, reinterpret_cast<const uint16_t*>(marker), marker_length, 4096, &children, collect_child); if (status != MIRAGE_OK) return mirage_status_to_ntstatus(status);
    *transferred = 0;
    for (const auto& child : children) { const auto name_bytes = child.name.size()*sizeof(wchar_t); std::vector<unsigned char> storage(sizeof(FSP_FSCTL_DIR_INFO)+name_bytes); auto* entry = reinterpret_cast<FSP_FSCTL_DIR_INFO*>(storage.data()); std::memset(entry,0,storage.size()); entry->Size=static_cast<UINT16>(sizeof(FSP_FSCTL_DIR_INFO)+name_bytes); fill_info(child.info,&entry->FileInfo,host(fs)->writable()); std::memcpy(entry->FileNameBuf,child.name.data(),name_bytes); if (!FspFileSystemAddDirInfo(entry,buffer,length,transferred)) break; }
    FspFileSystemAddDirInfo(nullptr,buffer,length,transferred); return STATUS_SUCCESS;
}
NTSTATUS read_file(FSP_FILE_SYSTEM* fs, PVOID context, PVOID buffer, UINT64 offset, ULONG length, PULONG transferred) {
    auto* opened=static_cast<FileContext*>(context); if(!opened||opened->info.directory)return STATUS_FILE_IS_A_DIRECTORY;
    // FspFileSystemOperationProcessId() is only valid during Create/Open/Rename; Read always sees 0.
    // Use the pid captured when this file was opened.
    const UINT32 pid=opened->opener_pid;
    auto* owner=host(fs);if(owner->async_delay_ms()!=0){
        const auto hint=FspFileSystemGetOperationContext()->Request->Hint;opened->references.fetch_add(1);owner->begin_pending();
        try{std::thread([fs,owner,opened,buffer,offset,length,hint,pid]{Sleep(owner->async_delay_ms());size_t read{};const auto result=mirage_read_ex(opened->rust_handle,offset,static_cast<uint8_t*>(buffer),length,&read,pid);FSP_FSCTL_TRANSACT_RSP response;std::memset(&response,0,sizeof(response));response.Size=sizeof(response);response.Kind=FspFsctlTransactReadKind;response.Hint=hint;response.IoStatus.Status=mirage_status_to_ntstatus(result);response.IoStatus.Information=static_cast<UINT32>(read);FspFileSystemSendResponse(fs,&response);mirage::release_file_context(opened);owner->end_pending();}).detach();return STATUS_PENDING;}catch(...){mirage::release_file_context(opened);owner->end_pending();}
    }
    size_t read{};const auto status=mirage_read_ex(opened->rust_handle,offset,static_cast<uint8_t*>(buffer),length,&read,pid);*transferred=static_cast<ULONG>(read);return mirage_status_to_ntstatus(status);
}
NTSTATUS set_basic_info(FSP_FILE_SYSTEM* fs,PVOID context,UINT32,UINT64 creation_time,UINT64,UINT64 last_write_time,UINT64,FSP_FSCTL_FILE_INFO* info){
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    if(!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    // Persist the caller's creation/modification stamps in the namespace;
    // attribute bits are accepted but not stored (content semantics are
    // unaffected) and access/change times track the write stamp.
    auto ft_to_ns = [](UINT64 ft) -> int64_t {
        if (ft <= 116444736000000000ULL) return 0;
        return int64_t((ft - 116444736000000000ULL) * 100);
    };
    int64_t created_ns = ft_to_ns(creation_time);
    int64_t modified_ns = ft_to_ns(last_write_time);
    if (created_ns || modified_ns) mirage_set_times(opened->rust_handle, created_ns, modified_ns);
    MirageFileInfo refreshed{}; if(mirage_file_stat(opened->rust_handle,&refreshed)==MIRAGE_OK) opened->info=refreshed;
    fill_info(opened->info,info,host(fs)->writable());
    return STATUS_SUCCESS;
}
NTSTATUS write_file(FSP_FILE_SYSTEM* fs,PVOID context,PVOID buffer,UINT64 offset,ULONG length,BOOLEAN write_to_end,BOOLEAN constrained,PULONG transferred,FSP_FSCTL_FILE_INFO* info){
    auto* opened=static_cast<FileContext*>(context); if(!opened||opened->info.directory) return STATUS_FILE_IS_A_DIRECTORY;
    if(!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    // WriteToEndOfFile: offset is the current EOF for a pure append.
    const uint64_t at = write_to_end ? opened->info.size : offset;
    // ConstrainedIo: writes may not extend past the current FileSize.
    if(constrained&&at>=opened->info.size){*transferred=0;fill_info(opened->info,info,host(fs)->writable());return STATUS_SUCCESS;}
    if(constrained&&static_cast<uint64_t>(length)>opened->info.size-at)length=static_cast<ULONG>(opened->info.size-at);
    size_t written{}; const auto status=mirage_write(opened->rust_handle,at,static_cast<const uint8_t*>(buffer),length,&written);
    *transferred=static_cast<ULONG>(written);
    if(status==MIRAGE_OK){ MirageFileInfo refreshed{}; if(mirage_file_stat(opened->rust_handle,&refreshed)==MIRAGE_OK) opened->info=refreshed; fill_info(opened->info,info,host(fs)->writable()); }
    return mirage_status_to_ntstatus(status);
}
NTSTATUS overwrite(FSP_FILE_SYSTEM* fs,PVOID context,UINT32,BOOLEAN,UINT64,FSP_FSCTL_FILE_INFO* info){
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    if(!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    // Overwrite/supersede collapses the file to empty; attribute replacement
    // is not implemented — the content contract is what callers depend on.
    const auto status=mirage_truncate(opened->rust_handle,0);
    if(status==MIRAGE_OK){ MirageFileInfo refreshed{}; if(mirage_file_stat(opened->rust_handle,&refreshed)==MIRAGE_OK) opened->info=refreshed; fill_info(opened->info,info,host(fs)->writable()); }
    return mirage_status_to_ntstatus(status);
}
NTSTATUS set_file_size(FSP_FILE_SYSTEM* fs,PVOID context,UINT64 new_size,BOOLEAN,FSP_FSCTL_FILE_INFO* info){
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    if(!host(fs)->writable()) return STATUS_MEDIA_WRITE_PROTECTED;
    const auto status=mirage_truncate(opened->rust_handle,new_size);
    if(status==MIRAGE_OK){ MirageFileInfo refreshed{}; if(mirage_file_stat(opened->rust_handle,&refreshed)==MIRAGE_OK) opened->info=refreshed; fill_info(opened->info,info,host(fs)->writable()); }
    return mirage_status_to_ntstatus(status);
}
NTSTATUS flush_file(FSP_FILE_SYSTEM* fs,PVOID context,FSP_FSCTL_FILE_INFO* info){
    auto* opened=static_cast<FileContext*>(context); if(!opened) return STATUS_INVALID_HANDLE;
    const auto status=mirage_flush(opened->rust_handle);
    if(status==MIRAGE_OK&&info){ MirageFileInfo refreshed{}; if(mirage_file_stat(opened->rust_handle,&refreshed)==MIRAGE_OK) opened->info=refreshed; fill_info(opened->info,info,host(fs)->writable()); }
    return mirage_status_to_ntstatus(status);
}
// Named-member assignment on a zero-initialized table: MSVC rejects
// designated initializers that do not follow FSP_FILE_SYSTEM_INTERFACE
// declaration order (C7560), and the order is SDK-version sensitive.
const FSP_FILE_SYSTEM_INTERFACE& winfsp_interface() {
    static const FSP_FILE_SYSTEM_INTERFACE table = [] {
        FSP_FILE_SYSTEM_INTERFACE t{};
        t.GetVolumeInfo=get_volume; t.GetSecurityByName=security_by_name;
        t.Create=create; t.Open=open_file; t.Overwrite=overwrite;
        t.Cleanup=cleanup; t.Close=close_file; t.Read=read_file; t.Write=write_file;
        t.Flush=flush_file; t.GetFileInfo=get_info; t.SetBasicInfo=set_basic_info; t.SetFileSize=set_file_size;
        t.Rename=rename_file; t.ReadDirectory=read_dir;
        t.SetDelete=set_delete;
        return t;
    }();
    return table;
}
}
namespace mirage {
void release_file_context(FileContext* context) noexcept{if(context&&context->references.fetch_sub(1)==1){mirage_file_close(context->rust_handle);delete context;}}
FileSystemHost::~FileSystemHost(){stop();if(stop_event_)CloseHandle(stop_event_);if(pending_zero_)CloseHandle(pending_zero_);if(engine_)mirage_engine_destroy(engine_);if(security_descriptor_)LocalFree(security_descriptor_);}
NTSTATUS FileSystemHost::mount(const std::wstring& path,const std::wstring& index_path,const std::wstring& storage_root,const std::wstring& owner_sid,bool cache_mode,bool writable,std::uint64_t volume_total_bytes,std::uint64_t volume_free_bytes,const std::wstring& origin_root,const std::wstring& drive_manifest,const std::wstring& repository_key,std::uint64_t disk_floor){
    if(volume_total_bytes==0||volume_free_bytes>volume_total_bytes)return STATUS_INVALID_PARAMETER;volume_total_bytes_=volume_total_bytes;volume_free_bytes_=volume_free_bytes;writable_=writable;
    stop_event_=CreateEventW(nullptr,TRUE,FALSE,nullptr);pending_zero_=CreateEventW(nullptr,TRUE,TRUE,nullptr);if(!stop_event_||!pending_zero_)return FspNtStatusFromWin32(GetLastError());wchar_t delay[32]{};if(GetEnvironmentVariableW(L"MIRAGE_TEST_ASYNC_DELAY_MS",delay,static_cast<DWORD>(std::size(delay))))async_delay_ms_=wcstoul(delay,nullptr,10);// Managed mode: --origin carries the local pack directory mirroring committed content (optional).
const bool dbg=GetEnvironmentVariableW(L"MIRAGE_DEBUG_PROVIDER",nullptr,0)>0;
auto ffi=writable?mirage_engine_create_managed_drive_at(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),origin_root.empty()?nullptr:reinterpret_cast<const uint16_t*>(origin_root.data()),origin_root.size(),dirty_budget_?dirty_budget_:volume_free_bytes,drive_manifest.empty()?nullptr:reinterpret_cast<const uint16_t*>(drive_manifest.data()),drive_manifest.size(),repository_key.empty()?nullptr:reinterpret_cast<const uint16_t*>(repository_key.data()),repository_key.size(),journal_root_.empty()?nullptr:reinterpret_cast<const uint16_t*>(journal_root_.data()),journal_root_.size(),&engine_):cache_mode?(origin_root.empty()?mirage_engine_create_cache(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),&engine_):mirage_engine_create_cache_with_origin(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),reinterpret_cast<const uint16_t*>(origin_root.data()),origin_root.size(),&engine_)):mirage_engine_create_local(reinterpret_cast<const uint16_t*>(index_path.data()),index_path.size(),reinterpret_cast<const uint16_t*>(storage_root.data()),storage_root.size(),&engine_);if(ffi!=MIRAGE_OK){if(dbg)std::wcerr<<L"engine create status=0x"<<std::hex<<(int)ffi<<L"\n";}if(ffi!=MIRAGE_OK)return mirage_status_to_ntstatus(ffi);if(dbg)std::wcerr<<L"engine create ok\n";if(disk_floor&&writable)mirage_engine_set_disk_floor(engine_,disk_floor);
    if(!create_mount_security(owner_sid,&security_descriptor_,&security_size_)){if(dbg)std::wcerr<<L"mount security failed win32="<<GetLastError()<<L"\n";return FspNtStatusFromWin32(GetLastError());}if(dbg)std::wcerr<<L"mount security ok\n";
    FSP_FSCTL_VOLUME_PARAMS params{};params.SectorSize=4096;params.SectorsPerAllocationUnit=1;params.CasePreservedNames=1;params.UnicodeOnDisk=1;params.PersistentAcls=1;params.ReadOnlyVolume=writable?0:1;wcscpy_s(params.FileSystemName,L"MirageSSD");
    // Descriptor semantics: without this flag WinFsp treats the pointer returned by Open as a
    // file *node* that must be identical for every concurrent open of the same file. We allocate
    // a fresh FileContext per open and free it in Close, so UserContext2 semantics are required;
    // node semantics would let a second open see a stale/freed context.
    params.UmFileContextIsUserContext2=1;
    // INFINITE FileInfoTimeout enables kernel data caching and read-ahead in
    // WinFsp — safe only while a mounted generation is immutable. A writable
    // managed volume must keep a finite timeout so mutations propagate to
    // other readers.
    params.Version=sizeof(params);params.FileInfoTimeout=writable?1000:INFINITE;params.VolumeInfoTimeoutValid=1;params.VolumeInfoTimeout=1000;params.DirInfoTimeoutValid=1;params.DirInfoTimeout=1000;
    auto status=FspFileSystemCreate(const_cast<PWSTR>(L"" FSP_FSCTL_DISK_DEVICE_NAME),&params,const_cast<FSP_FILE_SYSTEM_INTERFACE*>(&winfsp_interface()),&fs_);if(dbg)std::wcerr<<L"FspFileSystemCreate=0x"<<std::hex<<(unsigned long)status<<L"\n";if(!NT_SUCCESS(status))return status;fs_->UserContext=this;status=FspFileSystemSetMountPoint(fs_,const_cast<PWSTR>(path.c_str()));if(dbg)std::wcerr<<L"SetMountPoint=0x"<<std::hex<<(unsigned long)status<<L"\n";if(!NT_SUCCESS(status)){FspFileSystemDelete(fs_);fs_=nullptr;}return status;
}
NTSTATUS FileSystemHost::run(){if(!fs_)return STATUS_INVALID_DEVICE_STATE;
    // Compaction at startup is best-effort: a failure is logged, never fatal.
    if(engine_){const auto compact=mirage_engine_compact(engine_);if(compact!=MIRAGE_OK)std::cerr<<"compaction failed status="<<static_cast<int>(compact)<<"\n";}
    // Startup recovery must succeed before the dispatcher accepts I/O; a
    // failed reclaim leaves the coordinator Recovering and no READY line.
    if(engine_){const auto mark=mirage_engine_mark_mounted(engine_);if(mark!=MIRAGE_OK){std::cerr<<"recovery failed status="<<static_cast<int>(mark)<<"\n";return STATUS_UNSUCCESSFUL;}}
    const auto status=FspFileSystemStartDispatcher(fs_,0);if(!NT_SUCCESS(status))return status;std::cout<<"MIRAGE_READY\n"<<std::flush;WaitForSingleObject(stop_event_,INFINITE);return STATUS_SUCCESS;}
void FileSystemHost::set_drive_token(const std::string& token){if(engine_){const auto status=mirage_engine_set_drive_token(engine_,reinterpret_cast<const uint8_t*>(token.data()),token.size());if(status!=MIRAGE_OK)std::cerr<<"drive token rejected status="<<static_cast<int>(status)<<"\n";}}
void FileSystemHost::request_eviction(const std::string& bytes){std::uint64_t freed=0;std::uint64_t blocked=0;if(engine_){const auto status=mirage_engine_evict_published(engine_,std::strtoull(bytes.c_str(),nullptr,10),&freed,&blocked);if(status!=MIRAGE_OK){std::cerr<<"evict failed status="<<static_cast<int>(status)<<"\n";return;}}std::cout<<"MIRAGE_EVICTED "<<freed<<"\n"<<std::flush;}
void FileSystemHost::begin_pending() noexcept{if(pending_count_.fetch_add(1)==0)ResetEvent(pending_zero_);}
void FileSystemHost::end_pending() noexcept{if(pending_count_.fetch_sub(1)==1)SetEvent(pending_zero_);}
void FileSystemHost::stop() noexcept{if(fs_){if(WaitForSingleObject(pending_zero_,60000)==WAIT_TIMEOUT){const auto abandoned=pending_count_.load();std::cerr<<"stop: "<<abandoned<<" pending read(s) abandoned after 60s drain\n";}FspFileSystemStopDispatcher(fs_);FspFileSystemDelete(fs_);fs_=nullptr;}if(engine_){// Quiesced means no open handles and no extent version in use: safe to
    // compact superseded versions and reclaim dead payloads. A compaction
    // failure must never fail the unmount — log and leave data in place.
    if(mirage_engine_quiesce(engine_,10000)==MIRAGE_OK){const auto compact=mirage_engine_compact(engine_);if(compact!=MIRAGE_OK)std::cerr<<"compaction failed status="<<static_cast<int>(compact)<<"\n";}}if(stop_event_)SetEvent(stop_event_);}
}

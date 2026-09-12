use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use mirage_cache::ResidentIndex;
use mirage_index::{MountIndex, NodeIndex};
use mirage_pack::{PackReadEncryption, PackReader};
use mirage_types::PageHash;
pub struct DecodedPageCache {
    entries: BTreeMap<PageHash, Arc<[u8]>>,
    order: VecDeque<PageHash>,
    capacity: usize,
}
impl DecodedPageCache {
    pub fn bounded(capacity: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }
    pub fn get(&self, hash: PageHash) -> Option<Arc<[u8]>> {
        self.entries.get(&hash).cloned()
    }
    pub fn insert(&mut self, hash: PageHash, bytes: Arc<[u8]>) {
        if self.entries.contains_key(&hash) {
            return;
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(hash);
        self.entries.insert(hash, bytes);
    }
}
#[derive(Debug, Clone)]
pub struct Entry {
    pub index: u64,
    pub size: u64,
    pub directory: bool,
}
/// Append-only record of reads that hit a non-resident page in the cache-only engine.
///
/// ADR 0002 treats such reads as seal violations, so they are counted and written as
/// tab-separated lines (`<unix_ns>\tfile=<ordinal>\tpage=<ordinal>\toffset=<bytes>\tlen=<bytes>`)
/// on a best-effort basis; logging never fails a read.
pub struct ViolationLog {
    path: PathBuf,
    count: AtomicU64,
    origin_served: AtomicU64,
}
impl ViolationLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            count: AtomicU64::new(0),
            origin_served: AtomicU64::new(0),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        file_index: u32,
        page_ordinal: u32,
        offset: u64,
        length: usize,
        caller_pid: u32,
        path: &str,
        outcome: &str,
    ) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if outcome == "origin" {
            self.origin_served.fetch_add(1, Ordering::Relaxed);
        }
        self.append(&format!(
            "{}\tfile={file_index}\tpage={page_ordinal}\toffset={offset}\tlen={length}\tpid={caller_pid}\timage={}\tpath={path}\toutcome={outcome}\n",
            unix_ns(),
            caller_image_name(caller_pid)
        ));
    }
    /// Informational line recorded on every successful file lookup in the cache-only
    /// engine, so resolved file ordinals can be correlated with violation records.
    /// Does not count toward `count`.
    pub fn lookup(&self, file_index: u64, path: &str) {
        self.append(&format!(
            "{}\tlookup\tfile={file_index}\tpath={path}\tpid=0\n",
            unix_ns()
        ));
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
    pub fn origin_served(&self) -> u64 {
        self.origin_served.load(Ordering::Relaxed)
    }
    pub fn summary(&self) {
        self.append(&format!(
            "{}\tsummary\tnon_resident_reads={}\torigin_served={}\n",
            unix_ns(),
            self.count(),
            self.origin_served()
        ));
    }
    fn append(&self, line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}
/// Basename of the calling process image, resolved only on the violation path.
/// Returns `?` for pid 0 or on any lookup failure.
#[cfg(windows)]
fn caller_image_name(pid: u32) -> String {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    if pid == 0 {
        return "?".into();
    }
    // SAFETY: OpenProcess only needs the pid; the returned handle is closed on every path.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return "?".into();
    }
    let mut buffer = [0u16; 260];
    let mut length = buffer.len() as u32;
    // SAFETY: `process` is a live handle; `buffer` is writable for `length` UTF-16 units,
    // which is also the in/out capacity argument.
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };
    // SAFETY: `process` is a live handle owned by this call.
    unsafe { CloseHandle(process) };
    if ok == 0 || length == 0 {
        return "?".into();
    }
    let name = std::ffi::OsString::from_wide(&buffer[..length as usize]);
    Path::new(&name)
        .file_name()
        .map(|base| base.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}
#[cfg(not(windows))]
fn caller_image_name(_pid: u32) -> String {
    "?".into()
}
fn unix_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
pub struct MirageEngineHandle {
    pub entries: BTreeMap<Vec<u16>, Entry>,
    pub index: Option<Arc<MountIndex>>,
    pub object_root: Option<Arc<PathBuf>>,
    pub encryption: Option<PackReadEncryption>,
    pub readers: Arc<Mutex<BTreeMap<String, PackReader>>>,
    pub pages: Arc<Mutex<DecodedPageCache>>,
    pub resident: Option<Arc<ResidentIndex>>,
    pub violations: Option<Arc<ViolationLog>>,
    pub trace_lookups: bool,
}
pub struct MirageFileHandle {
    pub entry: Entry,
    pub index: Option<Arc<MountIndex>>,
    pub node: Option<NodeIndex>,
    pub object_root: Option<Arc<PathBuf>>,
    pub encryption: Option<PackReadEncryption>,
    pub readers: Arc<Mutex<BTreeMap<String, PackReader>>>,
    pub pages: Arc<Mutex<DecodedPageCache>>,
    pub resident: Option<Arc<ResidentIndex>>,
    pub violations: Option<Arc<ViolationLog>>,
}
impl MirageEngineHandle {
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
            index: None,
            object_root: None,
            encryption: None,
            readers: Arc::new(Mutex::new(BTreeMap::new())),
            pages: Arc::new(Mutex::new(DecodedPageCache::bounded(128))),
            resident: None,
            violations: None,
            trace_lookups: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ViolationLog;

    #[test]
    fn violation_log_counts_and_appends_lines() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("seal-violations.log");
        let log = ViolationLog::new(path.clone());
        log.record(7, 42, 44_040_192, 65_536, 0, "assets/pak0.pak", "origin");
        log.record(0, 1, 0, 4096, 0, "assets/pak0.pak", "failed");
        log.lookup(7, "assets/pak0.pak");
        log.summary();
        assert_eq!(log.count(), 2);
        assert_eq!(log.origin_served(), 1);
        let text = std::fs::read_to_string(&path).expect("log readable");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("\tfile=7\tpage=42\toffset=44040192\tlen=65536"));
        assert!(lines[0].ends_with("\tpid=0\timage=?\tpath=assets/pak0.pak\toutcome=origin"));
        assert!(lines[1].ends_with("\toutcome=failed"));
        assert!(lines[2].contains("\tlookup\tfile=7\tpath=assets/pak0.pak\tpid=0"));
        assert!(lines[3].ends_with("\tsummary\tnon_resident_reads=2\torigin_served=1"));
    }
}

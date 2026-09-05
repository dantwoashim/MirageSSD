use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
pub struct MirageEngineHandle {
    pub entries: BTreeMap<Vec<u16>, Entry>,
    pub index: Option<Arc<MountIndex>>,
    pub object_root: Option<Arc<PathBuf>>,
    pub encryption: Option<PackReadEncryption>,
    pub readers: Arc<Mutex<BTreeMap<String, PackReader>>>,
    pub pages: Arc<Mutex<DecodedPageCache>>,
    pub resident: Option<Arc<ResidentIndex>>,
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
        }
    }
}

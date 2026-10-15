//! Read-only publisher-chunk source for the native Mirage filesystem.
//!
//! Manifests are pinned local inputs, not remotely authenticated by this adapter.
//! Publisher MD5/length checks preserve that manifest's byte identity; they do
//! not create Mirage encrypted-sector proofs. Legacy cache seeds are read-only
//! and every reused chunk is checked, including holes and stale generations.
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::MirageStatus;
use crate::handles::Entry;
use crate::remote_cache::ByteLru;

mod cache;
pub mod install;
mod learning;
#[cfg(windows)]
mod process;
pub mod publisher;
pub mod retention;
#[cfg(test)]
mod tests;

const MAX_CHUNK: u64 = 4 * 1024 * 1024;
const MAX_COMPRESSED: u64 = 8 * 1024 * 1024;
type Key = [u8; 32];
type ChunkResult = Result<Arc<[u8]>, MirageStatus>;

#[derive(Deserialize)]
pub struct Config {
    pub manifests: Vec<PathBuf>,
    pub export_prefixes: Vec<String>,
    #[serde(default)]
    pub export_files: Vec<String>,
    /// Roots of existing rclone VFS data trees, never modified by Mirage.
    #[serde(default)]
    pub seed_roots: Vec<PathBuf>,
    pub cache_root: PathBuf,
    pub disk_budget_bytes: u64,
    pub disk_free_floor_bytes: u64,
    pub memory_budget_bytes: usize,
    #[serde(default)]
    pub retained_chunks: Vec<[u8; 32]>,
    /// Exact local executable eligible for access learning, independent of launcher polling.
    #[serde(default)]
    pub game_executable: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct Manifest {
    chunk_url_prefix: String,
    assets: Vec<Asset>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Asset {
    name: String,
    size: u64,
    md5: String,
    chunks: Vec<Chunk>,
    #[serde(skip)]
    prefix: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Chunk {
    name: String,
    md5: String,
    offset: u64,
    size: u64,
    dsize: u64,
}
struct Node {
    name: String,
    entry: Entry,
    asset: Option<Asset>,
    children: BTreeMap<String, u64>,
}
struct Flight {
    result: Mutex<Option<ChunkResult>>,
    ready: Condvar,
}
#[derive(Default, Serialize)]
pub struct Stats {
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub seed_hits: u64,
    pub cdn_chunks: u64,
    pub cdn_bytes: u64,
    pub failures: u64,
    pub cache_failures: u64,
    pub shared_waiters: u64,
    pub memory_bytes: usize,
    pub managed_disk_bytes: u64,
    pub retained_disk_bytes: u64,
    pub learned_chunks: usize,
}

pub struct SophonSource {
    nodes: Vec<Node>,
    pub entries: BTreeMap<Vec<u16>, Entry>,
    config: Config,
    client: reqwest::blocking::Client,
    memory: Mutex<ByteLru<Key, [u8]>>,
    seed_files: Mutex<ByteLru<Key, Mutex<File>>>,
    disk: Mutex<cache::DiskCache>,
    flights: Mutex<HashMap<Key, Arc<Flight>>>,
    owners: Mutex<usize>,
    capacity: Condvar,
    locals: Mutex<usize>,
    local_capacity: Condvar,
    stats: Mutex<Stats>,
    last_report: Mutex<Instant>,
    history: Mutex<learning::History>,
    learning: std::sync::atomic::AtomicU32,
    #[cfg(windows)]
    processes: Mutex<process::Classifier>,
    // One process owns disk bookkeeping; immutable seed trees need no lock.
    _lock: File,
}

fn valid_path(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.contains(['\\', '\0', '<', '>', '"', '|', '?', '*'])
        && !name.chars().any(char::is_control)
        && name.split('/').all(crate::safe_object_id)
}
fn valid_md5(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn verified(bytes: &[u8], chunk: &Chunk) -> bool {
    bytes.len() as u64 == chunk.dsize
        && format!("{:x}", Md5::digest(bytes)).eq_ignore_ascii_case(&chunk.md5)
}
fn chunk_key(asset: &Asset, chunk: &Chunk) -> Key {
    // Source representation AND expected plaintext identity, not a pathname.
    *blake3::hash(
        format!(
            "sophon-v1\0{}\0{}\0{}\0{}\0{}",
            asset.prefix,
            chunk.name,
            chunk.md5.to_ascii_lowercase(),
            chunk.size,
            chunk.dsize
        )
        .as_bytes(),
    )
    .as_bytes()
}

impl SophonSource {
    pub fn open(path: &Path) -> Result<Self, MirageStatus> {
        let bytes = std::fs::read(path).map_err(|_| MirageStatus::IoError)?;
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
        let config: Config =
            serde_json::from_slice(bytes).map_err(|_| MirageStatus::InvalidArgument)?;
        if config.manifests.is_empty()
            || (config.export_prefixes.is_empty() && config.export_files.is_empty())
            || config.export_files.iter().any(|name| !valid_path(name))
            || config.memory_budget_bytes > 256 * 1024 * 1024
            || config
                .export_prefixes
                .iter()
                .any(|p| !p.ends_with('/') || !valid_path(p.trim_end_matches('/')))
        {
            return Err(MirageStatus::InvalidArgument);
        }
        cache::ordinary_path(&config.cache_root)?;
        std::fs::create_dir_all(&config.cache_root).map_err(|_| MirageStatus::IoError)?;
        cache::ordinary_path(&config.cache_root.join("owner.lock"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config.cache_root.join("owner.lock"))
            .map_err(|_| MirageStatus::IoError)?;
        lock.try_lock().map_err(|_| MirageStatus::Conflict)?;
        retention::record_owner(&config.cache_root, path)?;
        let mut assets = BTreeMap::new();
        for path in &config.manifests {
            let bytes = std::fs::read(path).map_err(|_| MirageStatus::IoError)?;
            let manifest: Manifest =
                serde_json::from_slice(&bytes).map_err(|_| MirageStatus::IntegrityFailure)?;
            let prefix = reqwest::Url::parse(&manifest.chunk_url_prefix)
                .map_err(|_| MirageStatus::IntegrityFailure)?;
            if prefix.scheme() != "https"
                || prefix.host_str().is_none()
                || !prefix.username().is_empty()
                || prefix.password().is_some()
                || prefix.query().is_some()
                || prefix.fragment().is_some()
            {
                return Err(MirageStatus::IntegrityFailure);
            }
            for mut asset in manifest.assets {
                if !config
                    .export_prefixes
                    .iter()
                    .any(|p| asset.name.starts_with(p))
                    && !config.export_files.contains(&asset.name)
                {
                    continue;
                }
                if !valid_path(&asset.name) || !valid_md5(&asset.md5) {
                    return Err(MirageStatus::IntegrityFailure);
                }
                let mut end = 0u64;
                for chunk in &asset.chunks {
                    if !crate::safe_object_id(&chunk.name)
                        || !chunk
                            .name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        || !valid_md5(&chunk.md5)
                        || chunk.offset != end
                        || chunk.dsize == 0
                        || chunk.dsize > MAX_CHUNK
                        || chunk.size == 0
                        || chunk.size > MAX_COMPRESSED
                    {
                        return Err(MirageStatus::IntegrityFailure);
                    }
                    end = end
                        .checked_add(chunk.dsize)
                        .ok_or(MirageStatus::IntegrityFailure)?;
                }
                if end != asset.size {
                    return Err(MirageStatus::IntegrityFailure);
                }
                asset.prefix = manifest.chunk_url_prefix.trim_end_matches('/').to_string();
                let key = asset.name.to_lowercase();
                if assets.insert(key, asset).is_some() {
                    return Err(MirageStatus::IntegrityFailure);
                }
            }
        }
        if assets.is_empty() {
            return Err(MirageStatus::IntegrityFailure);
        }
        let mut tree = BTreeMap::<String, (String, Option<Asset>)>::new();
        tree.insert(String::new(), (String::new(), None));
        for (path, asset) in assets {
            let display = asset.name.clone();
            let parts: Vec<_> = display.split('/').collect();
            for depth in 1..parts.len() {
                let name = parts[..depth].join("/");
                let parent = tree.entry(name.to_lowercase()).or_insert((name, None));
                if parent.1.is_some() {
                    return Err(MirageStatus::IntegrityFailure);
                }
            }
            if tree.insert(path, (display, Some(asset))).is_some() {
                return Err(MirageStatus::IntegrityFailure);
            }
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut entries = BTreeMap::new();
        let mut paths = BTreeMap::new();
        for (path, (name, asset)) in tree {
            let index = nodes.len() as u64;
            let entry = Entry {
                index,
                size: asset.as_ref().map_or(0, |a| a.size),
                directory: asset.is_none(),
            };
            entries.insert(
                path.replace('/', "\\").encode_utf16().collect(),
                entry.clone(),
            );
            paths.insert(path, index);
            nodes.push(Node {
                name,
                entry,
                asset,
                children: BTreeMap::new(),
            });
        }
        for (path, index) in &paths {
            if path.is_empty() {
                continue;
            }
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            let parent = *paths.get(parent).ok_or(MirageStatus::IntegrityFailure)? as usize;
            if !nodes[parent].entry.directory {
                return Err(MirageStatus::IntegrityFailure);
            }
            nodes[parent]
                .children
                .insert(path.rsplit('/').next().unwrap().into(), *index);
        }
        let mut disk = cache::DiskCache::open(&config.cache_root, u64::MAX)?;
        retention::recover(&config.cache_root, &mut disk)?;
        if config.retained_chunks.len() > 8192 {
            return Err(MirageStatus::InvalidArgument);
        }
        let selected: std::collections::HashSet<_> =
            config.retained_chunks.iter().copied().collect();
        let sizes: HashMap<_, _> = nodes
            .iter()
            .filter_map(|node| node.asset.as_ref())
            .flat_map(|asset| {
                asset
                    .chunks
                    .iter()
                    .map(move |chunk| (chunk_key(asset, chunk), chunk.dsize))
            })
            .collect();
        let mut retained_bytes = 0u64;
        for key in selected {
            retained_bytes = retained_bytes
                .checked_add(
                    disk.estimated_size(*sizes.get(&key).ok_or(MirageStatus::IntegrityFailure)?),
                )
                .ok_or(MirageStatus::IntegrityFailure)?;
            if retained_bytes > config.disk_budget_bytes {
                return Err(MirageStatus::DiskFull);
            }
            disk.pin(key);
        }
        disk.set_budget(config.disk_budget_bytes)?;
        let mut identity = blake3::Hasher::new();
        for node in &nodes {
            if let Some(asset) = &node.asset {
                identity.update(asset.name.as_bytes());
                for chunk in &asset.chunks {
                    identity.update(&chunk_key(asset, chunk));
                }
            }
        }
        let history = learning::History::open(&config.cache_root, *identity.finalize().as_bytes())?;
        #[cfg(windows)]
        let processes = Mutex::new(process::Classifier::new(config.game_executable.as_deref()));
        Ok(Self {
            nodes,
            entries,
            memory: Mutex::new(ByteLru::new(config.memory_budget_bytes)),
            seed_files: Mutex::new(ByteLru::new(128)),
            disk: Mutex::new(disk),
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| MirageStatus::BackendUnavailable)?,
            config,
            flights: Mutex::new(HashMap::new()),
            owners: Mutex::new(0),
            capacity: Condvar::new(),
            locals: Mutex::new(0),
            local_capacity: Condvar::new(),
            stats: Mutex::new(Stats::default()),
            last_report: Mutex::new(Instant::now() - Duration::from_secs(3)),
            history: Mutex::new(history),
            learning: std::sync::atomic::AtomicU32::new(0),
            #[cfg(windows)]
            processes,
            _lock: lock,
        })
    }
    pub fn children(
        &self,
        index: u64,
        marker: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Entry)>, MirageStatus> {
        let node = self
            .nodes
            .get(index as usize)
            .ok_or(MirageStatus::NotFound)?;
        if !node.entry.directory {
            return Err(MirageStatus::InvalidArgument);
        }
        let marker = marker.map(str::to_lowercase);
        Ok(node
            .children
            .iter()
            .filter(|(name, _)| marker.as_ref().is_none_or(|m| *name > m))
            .take(limit)
            .map(|(_, i)| {
                let child = &self.nodes[*i as usize];
                (
                    child.name.rsplit('/').next().unwrap_or("").into(),
                    child.entry.clone(),
                )
            })
            .collect())
    }
    pub fn read(&self, index: u64, offset: u64, output: &mut [u8]) -> Result<usize, MirageStatus> {
        self.read_for_process(index, offset, output, 0)
    }
    pub fn read_for_process(
        &self,
        index: u64,
        offset: u64,
        output: &mut [u8],
        caller_pid: u32,
    ) -> Result<usize, MirageStatus> {
        let learning_pid = self.learning.load(std::sync::atomic::Ordering::Relaxed);
        let observe = learning_pid == u32::MAX || (learning_pid != 0 && learning_pid == caller_pid);
        #[cfg(windows)]
        let observe = if self.config.game_executable.is_some() {
            self.processes
                .lock()
                .is_ok_and(|mut processes| processes.is_game(caller_pid))
        } else {
            observe
        };
        let asset = self
            .nodes
            .get(index as usize)
            .and_then(|n| n.asset.as_ref())
            .ok_or(MirageStatus::InvalidArgument)?;
        if offset >= asset.size || output.is_empty() {
            return Ok(0);
        }
        let count = (asset.size - offset).min(output.len() as u64) as usize;
        let end = offset + count as u64;
        let first = asset
            .chunks
            .partition_point(|c| c.offset + c.dsize <= offset);
        let last = asset.chunks.partition_point(|c| c.offset < end);
        // Four useful chunks per wave. Global owner admission bounds all files;
        // no speculative queue can occupy foreground capacity.
        for wave in asset.chunks[first..last].chunks(4) {
            let results = if wave.len() == 1 {
                vec![self.get_chunk(asset, &wave[0], observe)]
            } else {
                std::thread::scope(|scope| {
                    let jobs: Vec<_> = wave
                        .iter()
                        .map(|chunk| scope.spawn(move || self.get_chunk(asset, chunk, observe)))
                        .collect();
                    jobs.into_iter()
                        .map(|job| job.join().unwrap_or(Err(MirageStatus::Internal)))
                        .collect::<Vec<_>>()
                })
            };
            for (chunk, result) in wave.iter().zip(results) {
                let bytes = result?;
                let start = offset.max(chunk.offset);
                let stop = end.min(chunk.offset + chunk.dsize);
                output[(start - offset) as usize..(stop - offset) as usize].copy_from_slice(
                    &bytes[(start - chunk.offset) as usize..(stop - chunk.offset) as usize],
                );
            }
        }
        self.report();
        Ok(count)
    }
    fn get_chunk(&self, asset: &Asset, chunk: &Chunk, observe: bool) -> ChunkResult {
        let started = Instant::now();
        let result = self.get_chunk_inner(asset, chunk);
        if result.is_ok() && observe {
            self.history
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .record(chunk_key(asset, chunk), chunk.dsize, started.elapsed());
        }
        result
    }
    fn get_chunk_inner(&self, asset: &Asset, chunk: &Chunk) -> ChunkResult {
        let key = chunk_key(asset, chunk);
        let (flight, owner) = {
            let mut flights = self.flights.lock().map_err(|_| MirageStatus::Internal)?;
            if let Some(bytes) = self
                .memory
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .get(&key)
            {
                self.stats.lock().unwrap().memory_hits += 1;
                return Ok(bytes);
            }
            if let Some(flight) = flights.get(&key) {
                (Arc::clone(flight), false)
            } else {
                let flight = Arc::new(Flight {
                    result: Mutex::new(None),
                    ready: Condvar::new(),
                });
                flights.insert(key, Arc::clone(&flight));
                (flight, true)
            }
        };
        if !owner {
            self.stats.lock().unwrap().shared_waiters += 1;
            let mut result = flight.result.lock().map_err(|_| MirageStatus::Internal)?;
            while result.is_none() {
                result = flight
                    .ready
                    .wait(result)
                    .map_err(|_| MirageStatus::Internal)?;
            }
            return result.as_ref().unwrap().clone();
        }
        // A panic must complete waiters and release admission, never strand a flight.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.load_chunk(asset, chunk, key)
        }))
        .unwrap_or(Err(MirageStatus::Internal));
        let mut flights = self.flights.lock().map_err(|_| MirageStatus::Internal)?;
        if let Ok(bytes) = &result {
            self.memory
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .insert(key, Arc::clone(bytes), bytes.len());
        } else {
            self.stats.lock().unwrap().failures += 1;
        }
        *flight.result.lock().map_err(|_| MirageStatus::Internal)? = Some(result.clone());
        flight.ready.notify_all();
        flights.remove(&key);
        result
    }
    fn load_chunk(&self, asset: &Asset, chunk: &Chunk, key: Key) -> ChunkResult {
        let local_permit = self.permit(false)?;
        let cached = self
            .disk
            .lock()
            .map_err(|_| MirageStatus::Internal)
            .and_then(|mut disk| disk.get(key, chunk));
        if let Ok(Some(bytes)) = cached {
            self.stats.lock().unwrap().disk_hits += 1;
            return Ok(bytes.into());
        }
        if cached.is_err() {
            self.stats.lock().unwrap().cache_failures += 1;
        }
        for root in &self.config.seed_roots {
            let path = root.join(&asset.name);
            let file_key = *blake3::hash(path.to_string_lossy().as_bytes()).as_bytes();
            let file = {
                let mut files = self.seed_files.lock().map_err(|_| MirageStatus::Internal)?;
                if let Some(file) = files.get(&file_key) {
                    file
                } else {
                    let Ok(file) = File::open(&path) else {
                        continue;
                    };
                    let file = Arc::new(Mutex::new(file));
                    files.insert(file_key, Arc::clone(&file), 1);
                    file
                }
            };
            let Ok(mut file) = file.lock() else {
                continue;
            };
            if file.seek(SeekFrom::Start(chunk.offset)).is_err() {
                continue;
            }
            let mut bytes = vec![0; chunk.dsize as usize];
            let read = file.read_exact(&mut bytes);
            drop(file);
            if read.is_ok() && verified(&bytes, chunk) {
                self.stats.lock().unwrap().seed_hits += 1;
                return Ok(bytes.into()); // Never duplicate a verified legacy seed on disk.
            }
        }
        drop(local_permit);
        let _network_permit = self.permit(true)?;
        let url = format!("{}/{}", asset.prefix, chunk.name);
        let mut response = self
            .client
            .get(url)
            .header("Accept-Encoding", "identity")
            .send()
            .map_err(|_| MirageStatus::BackendUnavailable)?;
        if response.status() != reqwest::StatusCode::OK
            || response.content_length().is_some_and(|n| n != chunk.size)
            || response
                .headers()
                .get("content-encoding")
                .is_some_and(|v| v != "identity")
        {
            return Err(MirageStatus::IntegrityFailure);
        }
        let mut compressed = Vec::with_capacity(chunk.size as usize);
        (&mut response)
            .take(chunk.size + 1)
            .read_to_end(&mut compressed)
            .map_err(|_| MirageStatus::BackendUnavailable)?;
        if compressed.len() as u64 != chunk.size {
            return Err(MirageStatus::IntegrityFailure);
        }
        let mut decoder = zstd::stream::read::Decoder::new(&compressed[..])
            .map_err(|_| MirageStatus::IntegrityFailure)?;
        decoder
            .window_log_max(23)
            .map_err(|_| MirageStatus::IntegrityFailure)?;
        let mut bytes = Vec::with_capacity(chunk.dsize as usize);
        decoder
            .take(chunk.dsize + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MirageStatus::IntegrityFailure)?;
        if !verified(&bytes, chunk) {
            return Err(MirageStatus::IntegrityFailure);
        }
        {
            let mut stats = self.stats.lock().unwrap();
            stats.cdn_chunks += 1;
            stats.cdn_bytes += compressed.len() as u64;
        }
        if self
            .disk
            .lock()
            .map_err(|_| MirageStatus::Internal)
            .and_then(|mut disk| disk.put(key, &bytes, self.config.disk_free_floor_bytes))
            .is_err()
        {
            self.stats.lock().unwrap().cache_failures += 1;
        }
        Ok(bytes.into())
    }
    fn permit(&self, remote: bool) -> Result<cache::Permit<'_>, MirageStatus> {
        let (owners, capacity) = if remote {
            (&self.owners, &self.capacity)
        } else {
            (&self.locals, &self.local_capacity)
        };
        let mut owners = owners.lock().map_err(|_| MirageStatus::Internal)?;
        while *owners >= 8 {
            owners = capacity.wait(owners).map_err(|_| MirageStatus::Internal)?;
        }
        *owners += 1;
        Ok(cache::Permit {
            source: self,
            remote,
        })
    }
    pub fn statistics(&self) -> Stats {
        // Lock ordering follows get_chunk: memory -> stats; never hold stats
        // while acquiring a residency lock.
        let memory_bytes = self.memory.lock().unwrap().used();
        let (managed_disk_bytes, retained_disk_bytes) = {
            let disk = self.disk.lock().unwrap();
            (disk.used(), disk.retained_bytes())
        };
        let learned_chunks = self.history.lock().unwrap().len();
        let s = self.stats.lock().unwrap();
        Stats {
            memory_hits: s.memory_hits,
            disk_hits: s.disk_hits,
            seed_hits: s.seed_hits,
            cdn_chunks: s.cdn_chunks,
            cdn_bytes: s.cdn_bytes,
            failures: s.failures,
            cache_failures: s.cache_failures,
            shared_waiters: s.shared_waiters,
            memory_bytes,
            managed_disk_bytes,
            retained_disk_bytes,
            learned_chunks,
        }
    }
    fn report(&self) {
        let Ok(mut last) = self.last_report.try_lock() else {
            return;
        };
        if last.elapsed() < Duration::from_secs(2) {
            return;
        }
        *last = Instant::now();
        if let Ok(history) = self.history.lock() {
            let _ = history.save(&self.config.cache_root);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.statistics()) {
            let temp = self.config.cache_root.join("stats.tmp");
            if std::fs::write(&temp, bytes).is_ok() {
                let _ = std::fs::rename(temp, self.config.cache_root.join("stats.json"));
            }
        }
    }

    /// Explicit learning control for legacy sources and controlled fixtures.
    /// Managed Windows sources instead verify each opener against game_executable.
    pub fn set_gameplay_learning(&self, enabled: bool) -> Result<(), MirageStatus> {
        self.set_gameplay_process(if enabled { u32::MAX } else { 0 })
    }
    pub fn set_gameplay_process(&self, pid: u32) -> Result<(), MirageStatus> {
        self.learning
            .store(pid, std::sync::atomic::Ordering::Relaxed);
        if pid == 0 {
            self.history
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .save(&self.config.cache_root)?;
        }
        Ok(())
    }

    /// Prepare the learned set before handing off to the publisher launcher.
    /// Explicit byte budget includes retained pins; no speculative work steals
    /// foreground fetch slots. A seed is verified but never duplicated.
    pub fn prepare_learned(&self, byte_budget: u64) -> Result<Vec<[u8; 32]>, MirageStatus> {
        if byte_budget > self.config.disk_budget_bytes {
            return Err(MirageStatus::InvalidArgument);
        }
        let selected = self
            .history
            .lock()
            .map_err(|_| MirageStatus::Internal)?
            .select(byte_budget);
        let lookup: HashMap<_, _> = self
            .nodes
            .iter()
            .filter_map(|n| n.asset.as_ref())
            .flat_map(|asset| {
                asset
                    .chunks
                    .iter()
                    .map(move |chunk| (chunk_key(asset, chunk), (asset, chunk)))
            })
            .collect();
        let mut prepared = Vec::new();
        let mut remaining = byte_budget;
        for key in selected {
            let Some((asset, chunk)) = lookup.get(&key) else {
                continue;
            };
            let allocation = self
                .disk
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .estimated_size(chunk.dsize);
            if allocation > remaining {
                continue;
            }
            remaining -= allocation;
            self.disk
                .lock()
                .map_err(|_| MirageStatus::Internal)?
                .pin(key);
            self.get_chunk_inner(asset, chunk)?;
            prepared.push(key);
        }
        Ok(prepared)
    }
}

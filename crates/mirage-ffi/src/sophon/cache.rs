use super::*;

pub(super) struct Permit<'a> {
    pub source: &'a SophonSource,
    pub remote: bool,
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let (owners, capacity) = if self.remote {
            (&self.source.owners, &self.source.capacity)
        } else {
            (&self.source.locals, &self.source.local_capacity)
        };
        if let Ok(mut owners) = owners.lock() {
            *owners -= 1;
            capacity.notify_one();
        }
    }
}
pub(super) struct DiskCache {
    root: PathBuf,
    entries: HashMap<Key, (u64, u64)>,
    budget: u64,
    used: u64,
    tick: u64,
}
fn filename(key: Key) -> String {
    format!(
        "{}.chunk",
        key.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}
fn parse_key(name: &str) -> Option<Key> {
    let name = name.strip_suffix(".chunk")?;
    if name.len() != 64 {
        return None;
    }
    let mut key = [0; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}
impl DiskCache {
    pub fn open(root: &Path, budget: u64) -> Result<Self, MirageStatus> {
        let root = root.join("chunks-v1");
        std::fs::create_dir_all(&root).map_err(|_| MirageStatus::IoError)?;
        let mut entries = HashMap::new();
        let mut used = 0u64;
        let mut files = Vec::new();
        for item in std::fs::read_dir(&root).map_err(|_| MirageStatus::IoError)? {
            let item = item.map_err(|_| MirageStatus::IoError)?;
            if item
                .file_type()
                .map_err(|_| MirageStatus::IoError)?
                .is_file()
                && let Some(key) = item.file_name().to_str().and_then(parse_key)
            {
                let meta = item.metadata().map_err(|_| MirageStatus::IoError)?;
                files.push((meta.modified().ok(), key, meta.len()));
            }
        }
        files.sort_by_key(|(time, _, _)| *time);
        for (_, key, length) in files {
            entries.insert(key, (length, entries.len() as u64));
            used = used
                .checked_add(length)
                .ok_or(MirageStatus::IntegrityFailure)?;
        }
        let tick = entries.len() as u64;
        let mut cache = Self {
            root,
            entries,
            budget,
            used,
            tick,
        };
        cache.make_room(0)?;
        Ok(cache)
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    fn remove(&mut self, key: Key) -> Result<(), MirageStatus> {
        let path = self.root.join(filename(key));
        // Only hash-named owned chunks are disposable. Seed/original paths
        // are never passed to this method.
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(MirageStatus::IoError),
        }
        if let Some((size, _)) = self.entries.remove(&key) {
            self.used -= size;
        }
        Ok(())
    }
    fn make_room(&mut self, needed: u64) -> Result<(), MirageStatus> {
        while self.used > self.budget.saturating_sub(needed) {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, tick))| tick)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.remove(key)?;
        }
        Ok(())
    }
    pub fn get(&mut self, key: Key, chunk: &Chunk) -> Result<Option<Vec<u8>>, MirageStatus> {
        if !self.entries.contains_key(&key) {
            return Ok(None);
        }
        let path = self.root.join(filename(key));
        let bytes = File::open(path).and_then(|f| {
            let mut bytes = Vec::with_capacity(chunk.dsize as usize);
            f.take(chunk.dsize + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        if let Ok(bytes) = bytes
            && verified(&bytes, chunk)
        {
            self.tick = self.tick.saturating_add(1);
            if let Some((_, tick)) = self.entries.get_mut(&key) {
                *tick = self.tick;
            }
            return Ok(Some(bytes));
        }
        self.remove(key)?;
        Ok(None)
    }
    pub fn put(&mut self, key: Key, bytes: &[u8], floor: u64) -> Result<(), MirageStatus> {
        let length = bytes.len() as u64;
        if length > self.budget || self.entries.contains_key(&key) {
            return Ok(());
        }
        self.make_room(length)?;
        if !space_available(&self.root, floor.saturating_add(length)) {
            return Ok(());
        }
        let temp = self.root.join("placement.tmp");
        let placed = (|| -> std::io::Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, self.root.join(filename(key)))
        })();
        if placed.is_err() {
            let _ = std::fs::remove_file(&temp);
            // Cache placement is optional; verified transient bytes still
            // satisfy the read if disk fills or cache storage fails.
            return Ok(());
        }
        self.tick = self.tick.saturating_add(1);
        self.entries.insert(key, (length, self.tick));
        self.used += length;
        Ok(())
    }
}
#[cfg(windows)]
fn space_available(path: &Path, needed: u64) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: NUL-terminated path and one writable u64; other results unused.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    ok != 0 && available >= needed
}
#[cfg(not(windows))]
fn space_available(_path: &Path, needed: u64) -> bool {
    // The adapter targets Windows. Other targets must not claim a floor
    // they cannot measure; test caches explicitly disable it.
    needed <= MAX_CHUNK
}

use super::*;

const MAX_ENTRIES: usize = 8192;
const MAX_PROFILE: u64 = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Access {
    key: Key,
    bytes: u64,
    count: u64,
    tick: u64,
    cost_us: u64,
}
#[derive(Serialize, Deserialize)]
pub(super) struct History {
    generation: Key,
    tick: u64,
    entries: Vec<Access>,
}
impl History {
    pub fn open(root: &Path, generation: Key) -> Result<Self, MirageStatus> {
        let fresh = || Self {
            generation,
            tick: 0,
            entries: Vec::new(),
        };
        let path = root.join("access-profile-v1.json");
        cache::ordinary_path(&path)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(fresh()),
            Err(_) => return Err(MirageStatus::IoError),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PROFILE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MirageStatus::IoError)?;
        if bytes.len() as u64 > MAX_PROFILE {
            return Err(MirageStatus::IntegrityFailure);
        }
        let history: Self =
            serde_json::from_slice(&bytes).map_err(|_| MirageStatus::IntegrityFailure)?;
        if history.entries.len() > MAX_ENTRIES
            || history.entries.iter().any(|e| {
                e.bytes == 0 || e.bytes > MAX_CHUNK || e.tick > history.tick || e.count == 0
            })
        {
            return Err(MirageStatus::IntegrityFailure);
        }
        if history.generation == generation {
            Ok(history)
        } else {
            Ok(fresh())
        }
    }
    pub fn record(&mut self, key: Key, bytes: u64, elapsed: Duration) {
        self.tick = self.tick.saturating_add(1);
        let cost = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.count = entry.count.saturating_add(1);
            entry.tick = self.tick;
            // Decaying measured fetch/read cost; arithmetic remains bounded.
            entry.cost_us = entry
                .cost_us
                .saturating_sub(entry.cost_us / 8)
                .saturating_add(cost / 8);
        } else {
            if self.entries.len() == MAX_ENTRIES {
                let oldest = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.tick)
                    .unwrap()
                    .0;
                self.entries.swap_remove(oldest);
            }
            self.entries.push(Access {
                key,
                bytes,
                count: 1,
                tick: self.tick,
                cost_us: cost,
            });
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn select(&self, budget: u64) -> Vec<Key> {
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort_by(|a, b| {
            let score = |entry: &Access| {
                u128::from(entry.count.min(1_000_000)) * u128::from(entry.cost_us.max(1)) * 1024
                    / u128::from(entry.bytes)
            };
            score(b)
                .cmp(&score(a))
                .then_with(|| b.tick.cmp(&a.tick))
                .then_with(|| a.key.cmp(&b.key))
        });
        let mut remaining = budget;
        entries
            .into_iter()
            .filter_map(|entry| {
                if entry.bytes <= remaining {
                    remaining -= entry.bytes;
                    Some(entry.key)
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn save(&self, root: &Path) -> Result<(), MirageStatus> {
        let temp = root.join("access-profile.tmp");
        let target = root.join("access-profile-v1.json");
        cache::ordinary_path(&temp)?;
        cache::ordinary_path(&target)?;
        let bytes = serde_json::to_vec(self).map_err(|_| MirageStatus::Internal)?;
        if bytes.len() as u64 > MAX_PROFILE {
            return Err(MirageStatus::IntegrityFailure);
        }
        let mut file = File::create(&temp).map_err(|_| MirageStatus::IoError)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| MirageStatus::IoError)?;
        drop(file);
        std::fs::rename(temp, target).map_err(|_| MirageStatus::IoError)
    }
}

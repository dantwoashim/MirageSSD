//! Byte-bounded residency; immutable Arcs remain usable after eviction.
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

pub(crate) struct ByteLru<K, V: ?Sized> {
    entries: HashMap<K, (Arc<V>, usize, u64)>,
    used: usize,
    budget: usize,
    clock: u64,
}
impl<K: Copy + Eq + Hash, V: ?Sized> ByteLru<K, V> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            used: 0,
            budget,
            clock: 0,
        }
    }
    pub(crate) fn get(&mut self, key: &K) -> Option<Arc<V>> {
        let (value, _, touched) = self.entries.get_mut(key)?;
        self.clock = self.clock.saturating_add(1);
        *touched = self.clock;
        Some(Arc::clone(value))
    }
    pub(crate) fn insert(&mut self, key: K, value: Arc<V>, bytes: usize) {
        if bytes > self.budget {
            return;
        }
        if let Some((_, old, _)) = self.entries.remove(&key) {
            self.used -= old;
        }
        while self.used > self.budget - bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, touched))| touched)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some((_, old, _)) = self.entries.remove(&oldest) {
                self.used -= old;
            }
        }
        self.clock = self.clock.saturating_add(1);
        self.entries.insert(key, (value, bytes, self.clock));
        self.used += bytes;
    }
    pub(crate) fn used(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eviction_preserves_recent_entries_and_inflight_readers() {
        let mut cache = ByteLru::<u8, [u8]>::new(8);
        let held: Arc<[u8]> = Arc::from([1u8; 4]);
        cache.insert(1, Arc::clone(&held), 4);
        cache.insert(2, Arc::from([2u8; 4]), 4);
        assert!(Arc::ptr_eq(&cache.get(&1).unwrap(), &held));
        cache.insert(3, Arc::from([3u8; 4]), 4);
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&1).is_some());
        assert_eq!(&*held, &[1u8; 4]);
        cache.insert(4, Arc::from([4u8; 9]), 9);
        assert_eq!(cache.used(), 8);
        cache.insert(1, Arc::from([5u8; 2]), 2);
        assert_eq!(cache.used(), 6, "replacement must charge bytes once");
    }
}

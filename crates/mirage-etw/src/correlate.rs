use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub timestamp_100ns: u64,
    pub process_id: u32,
    pub path: Option<PathBuf>,
    pub offset: u64,
    pub size: u32,
    pub write: bool,
}

#[derive(Debug)]
pub struct Correlator {
    capacity: usize,
    names: HashMap<u64, PathBuf>,
    order: VecDeque<u64>,
}
impl Correlator {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            names: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    pub fn name(&mut self, file_key: u64, path: PathBuf) {
        if !self.names.contains_key(&file_key) {
            self.order.push_back(file_key);
        }
        self.names.insert(file_key, path);
        while self.names.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.names.remove(&old);
            }
        }
    }
    pub fn remove(&mut self, file_key: u64) {
        self.names.remove(&file_key);
    }
    pub fn io(
        &self,
        timestamp_100ns: u64,
        process_id: u32,
        file_key: u64,
        offset: u64,
        size: u32,
        write: bool,
    ) -> TraceEvent {
        TraceEvent {
            timestamp_100ns,
            process_id,
            path: self.names.get(&file_key).cloned(),
            offset,
            size,
            write,
        }
    }
}

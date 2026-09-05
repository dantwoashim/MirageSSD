use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time_100ns: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedRole {
    Root,
    Game,
    Helper,
    AntiCheat,
}
#[derive(Debug)]
pub struct ProcessTracker {
    processes: BTreeMap<ProcessIdentity, TrackedRole>,
    root: ProcessIdentity,
    root_exit: Option<SystemTime>,
    drain: Duration,
}
impl ProcessTracker {
    pub fn new(root: ProcessIdentity, drain: Duration) -> Self {
        Self {
            processes: [(root, TrackedRole::Root)].into(),
            root,
            root_exit: None,
            drain: drain.min(Duration::from_secs(120)),
        }
    }
    pub fn observe_start(&mut self, identity: ProcessIdentity, role: TrackedRole) {
        self.processes.insert(identity, role);
    }
    pub fn observe_exit(&mut self, identity: ProcessIdentity, now: SystemTime) {
        if identity == self.root {
            self.root_exit = Some(now);
        }
        self.processes.remove(&identity);
    }
    pub fn contains_pid_instance(&self, identity: ProcessIdentity) -> bool {
        self.processes.contains_key(&identity)
    }
    pub fn active_count(&self) -> usize {
        self.processes.len()
    }
    pub fn can_release_leases(&self, now: SystemTime) -> bool {
        if !self.processes.is_empty() {
            return false;
        }
        self.root_exit
            .is_some_and(|exit| now.duration_since(exit).unwrap_or_default() >= self.drain)
    }
}

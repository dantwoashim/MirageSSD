use mirage_service::{ProcessIdentity, ProcessTracker, TrackedRole};
use std::time::{Duration, SystemTime};
#[test]
fn root_exit_helpers_pid_reuse_and_drain_protect_leases() {
    let root = ProcessIdentity {
        pid: 10,
        creation_time_100ns: 1,
    };
    let helper = ProcessIdentity {
        pid: 11,
        creation_time_100ns: 2,
    };
    let mut tracker = ProcessTracker::new(root, Duration::from_secs(5));
    tracker.observe_start(helper, TrackedRole::Helper);
    let now = SystemTime::now();
    tracker.observe_exit(root, now);
    assert!(!tracker.can_release_leases(now + Duration::from_secs(20)));
    assert!(!tracker.contains_pid_instance(ProcessIdentity {
        pid: 11,
        creation_time_100ns: 3
    }));
    tracker.observe_exit(helper, now + Duration::from_secs(1));
    assert!(!tracker.can_release_leases(now + Duration::from_secs(4)));
    assert!(tracker.can_release_leases(now + Duration::from_secs(5)));
}

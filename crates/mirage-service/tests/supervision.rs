use mirage_service::{HostExit, HostId, HostSpec, HostState, Launcher, ManagedChild, Supervisor};
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

struct Child {
    exits: VecDeque<Option<HostExit>>,
    lines: Arc<Mutex<Vec<String>>>,
}
impl ManagedChild for Child {
    fn wait_ready(&mut self, _: std::time::Duration) -> io::Result<bool> {
        Ok(true)
    }
    fn try_exit(&mut self) -> io::Result<Option<HostExit>> {
        Ok(self.exits.pop_front().flatten())
    }
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        self.lines.lock().unwrap().push(line.to_owned());
        Ok(())
    }
    fn stop(&mut self) -> io::Result<HostExit> {
        Ok(HostExit {
            success: true,
            code: Some(0),
        })
    }
}
type Plans = VecDeque<VecDeque<Option<HostExit>>>;
#[derive(Clone)]
struct Mock {
    plans: Arc<Mutex<Plans>>,
    lines: Arc<Mutex<Vec<String>>>,
}
impl Launcher for Mock {
    type Child = Child;
    fn launch(&self, _: &HostSpec) -> io::Result<Child> {
        Ok(Child {
            exits: self.plans.lock().unwrap().pop_front().unwrap(),
            lines: Arc::clone(&self.lines),
        })
    }
}
fn spec() -> HostSpec {
    HostSpec {
        executable: "host".into(),
        mount_point: "mount".into(),
        index: "index".into(),
        state_root: "state".into(),
        owner_sid: "S-1-5-21-111".into(),
        volume_total_bytes: 5 * 1024 * 1024 * 1024 * 1024,
        volume_free_bytes: 4 * 1024 * 1024 * 1024 * 1024,
        origin_root: None,
        managed: false,
        drive_manifest: None,
        repository_key: None,
    }
}

#[test]
fn one_crash_is_isolated_and_reported_exactly() {
    let plans = VecDeque::from([
        VecDeque::from([
            None,
            Some(HostExit {
                success: false,
                code: Some(23),
            }),
        ]),
        VecDeque::from([None, None]),
    ]);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor = Supervisor::new(Mock {
        plans: Arc::new(Mutex::new(plans)),
        lines: Arc::clone(&lines),
    });
    let first = HostId::new("first").unwrap();
    let second = HostId::new("second").unwrap();
    supervisor.start(first.clone(), &spec()).unwrap();
    supervisor.start(second.clone(), &spec()).unwrap();
    assert!(
        supervisor
            .wait_ready(&first, std::time::Duration::ZERO)
            .unwrap()
    );
    assert!(supervisor.poll().unwrap().is_empty());
    let changes = supervisor.poll().unwrap();
    assert_eq!(
        changes,
        vec![(
            first.clone(),
            HostState::Crashed(HostExit {
                success: false,
                code: Some(23)
            })
        )]
    );
    assert_eq!(supervisor.state(&second), Some(HostState::Running));
    assert_eq!(supervisor.stop(&second).unwrap(), HostState::Stopped);
}

#[test]
fn send_line_writes_to_the_live_hosts_stdin() {
    let plans = VecDeque::from([VecDeque::from([None])]);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor = Supervisor::new(Mock {
        plans: Arc::new(Mutex::new(plans)),
        lines: Arc::clone(&lines),
    });
    let id = HostId::new("drive-repo").unwrap();
    supervisor.start(id.clone(), &spec()).unwrap();
    assert!(
        supervisor
            .wait_ready(&id, std::time::Duration::ZERO)
            .unwrap()
    );
    supervisor.send_line(&id, "TOKEN test-bearer").unwrap();
    assert_eq!(*lines.lock().unwrap(), vec!["TOKEN test-bearer"]);
    // A stopped host rejects further control writes.
    assert_eq!(supervisor.stop(&id).unwrap(), HostState::Stopped);
    assert!(supervisor.send_line(&id, "TOKEN later").is_err());
}

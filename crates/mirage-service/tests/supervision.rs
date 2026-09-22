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
        disk_floor: None,
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

#[test]
fn request_eviction_parses_mirage_evicted_and_times_out() {
    use std::sync::mpsc;
    use std::time::Duration;

    struct EvictChild {
        replies: mpsc::Receiver<String>,
        lines: Arc<Mutex<Vec<String>>>,
    }
    impl ManagedChild for EvictChild {
        fn wait_ready(&mut self, _: Duration) -> io::Result<bool> {
            Ok(true)
        }
        fn try_exit(&mut self) -> io::Result<Option<HostExit>> {
            Ok(None)
        }
        fn send_line(&mut self, line: &str) -> io::Result<()> {
            self.lines.lock().unwrap().push(line.to_owned());
            Ok(())
        }
        fn read_stdout_line(&mut self, timeout: Duration) -> io::Result<String> {
            self.replies
                .recv_timeout(timeout)
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no reply"))
        }
        fn stop(&mut self) -> io::Result<HostExit> {
            Ok(HostExit {
                success: true,
                code: Some(0),
            })
        }
    }
    struct EvictLauncher {
        lines: Arc<Mutex<Vec<String>>>,
        reply_tx: Arc<Mutex<mpsc::Sender<String>>>,
    }
    impl Launcher for EvictLauncher {
        type Child = EvictChild;
        fn launch(&self, _: &HostSpec) -> io::Result<EvictChild> {
            let (tx, rx) = mpsc::channel();
            *self.reply_tx.lock().unwrap() = tx;
            Ok(EvictChild {
                replies: rx,
                lines: Arc::clone(&self.lines),
            })
        }
    }

    let lines = Arc::new(Mutex::new(Vec::new()));
    let reply_tx = Arc::new(Mutex::new(mpsc::channel().0));
    let mut supervisor = Supervisor::new(EvictLauncher {
        lines: Arc::clone(&lines),
        reply_tx: Arc::clone(&reply_tx),
    });
    let id = HostId::new("evict").unwrap();
    supervisor.start(id.clone(), &spec()).unwrap();

    let responder = reply_tx.lock().unwrap().clone();
    responder.send("unrelated".to_owned()).unwrap();
    responder.send("MIRAGE_EVICTED 4096".to_owned()).unwrap();
    let (freed, blocked) = supervisor
        .request_eviction(&id, 8192, Duration::from_secs(5))
        .expect("eviction reply");
    assert_eq!(freed, 4096);
    assert_eq!(blocked, 0);

    // Extended reply: freed + pin-blocked bytes.
    responder
        .send("MIRAGE_EVICTED 2048 1024".to_owned())
        .unwrap();
    let (freed, blocked) = supervisor
        .request_eviction(&id, 8192, Duration::from_secs(5))
        .expect("extended eviction reply");
    assert_eq!((freed, blocked), (2048, 1024));
    assert_eq!(*lines.lock().unwrap(), vec!["EVICT 8192", "EVICT 8192"]);

    // No reply queued → timeout error.
    let timeout = supervisor.request_eviction(&id, 1, Duration::from_millis(150));
    assert_eq!(timeout.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        *lines.lock().unwrap(),
        vec!["EVICT 8192", "EVICT 8192", "EVICT 1"]
    );
}

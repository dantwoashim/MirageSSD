#![cfg(windows)]

use mirage_etw::EtwSession;

#[test]
fn kernel_file_session_starts_and_always_stops() {
    let name = format!("MirageSSD-ETW-test-{}", std::process::id());
    let session = match EtwSession::start_capture(&name, 4, 16, 65_536) {
        Ok(session) => session,
        Err(error) => {
            assert!(error.message.contains("elevation"));
            return;
        }
    };
    let duplicate = EtwSession::start(&name, 4, 16).expect_err("duplicate must fail");
    assert!(duplicate.message.contains("already exists"));
    let executable = std::env::current_exe().unwrap();
    std::fs::read(&executable).unwrap();
    let capture = session.stop_capture().expect("stop ETW session");
    assert_eq!(capture.metrics.events_lost, 0);
    assert!(
        capture.events.iter().any(|event| {
            !event.write
                && event.size > 0
                && event
                    .path
                    .as_ref()
                    .is_some_and(|path| path.file_name() == executable.file_name())
        }),
        "the real-time consumer did not capture the test executable read; captured={}, unknown={}, dropped={}",
        capture.events.len(),
        capture.unknown_paths,
        capture.dropped_events,
    );
}

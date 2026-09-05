use std::{
    path::Path,
    process::{Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct KilledProcess {
    pub status: ExitStatus,
    pub ready_after: Duration,
}

/// Starts a child, waits for its durable-boundary marker, then terminates it.
///
/// The marker must be created by the child only after the state under test has
/// been flushed. This keeps crash scenarios deterministic instead of relying on
/// timing races.
pub fn kill_after_ready(
    command: &mut Command,
    ready_marker: &Path,
    timeout: Duration,
) -> std::io::Result<KilledProcess> {
    let started = Instant::now();
    let mut child = command.spawn()?;
    while !ready_marker.is_file() {
        if let Some(status) = child.try_wait()? {
            return Err(std::io::Error::other(format!(
                "crash worker exited before readiness marker: {status}"
            )));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "crash worker readiness timed out",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    let ready_after = started.elapsed();
    child.kill()?;
    let status = child.wait()?;
    Ok(KilledProcess {
        status,
        ready_after,
    })
}

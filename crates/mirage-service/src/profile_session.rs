use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mirage_etw::{EtwSession, correlate::TraceEvent};
use mirage_types::MirageError;

#[derive(Debug, Clone)]
pub struct ProfileLaunch {
    pub game_root: PathBuf,
    pub launcher: PathBuf,
    pub arguments: Vec<String>,
    pub maximum_runtime: Duration,
    pub drain_interval: Duration,
    pub version_label: String,
    pub configuration_label: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileResult {
    pub process_id: u32,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed: Duration,
    pub etw_events_lost: u32,
    pub etw_buffers_lost: u32,
    pub unknown_path_events: u64,
    pub dropped_capture_events: u64,
    pub events: Vec<TraceEvent>,
    pub version_label: String,
    pub configuration_label: String,
}

pub fn run_profile_session(launch: &ProfileLaunch) -> Result<ProfileResult, MirageError> {
    if launch.arguments.len() > 256
        || launch.arguments.iter().any(|a| a.len() > 32_767)
        || launch.version_label.is_empty()
        || launch.configuration_label.is_empty()
        || launch.maximum_runtime.is_zero()
        || launch.maximum_runtime > Duration::from_secs(21_600)
    {
        return Err(MirageError::invalid_argument(
            "profile launch metadata is invalid",
        ));
    }
    let root = launch.game_root.canonicalize().map_err(MirageError::from)?;
    let launcher = launch.launcher.canonicalize().map_err(MirageError::from)?;
    if !launcher.starts_with(&root) || !launcher.is_file() {
        return Err(MirageError::invalid_argument(
            "profile launcher is outside selected game root",
        ));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let session_name = format!("MirageSSD-Profile-{}-{nonce}", std::process::id());
    let etw = EtwSession::start_capture(&session_name, 8, 64, 1_000_000)?;
    let started = Instant::now();
    let mut child = Command::new(&launcher)
        .args(&launch.arguments)
        .current_dir(&root)
        .spawn()
        .map_err(MirageError::from)?;
    let process_id = child.id();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MirageError::from(error));
            }
        }
        if started.elapsed() >= launch.maximum_runtime {
            timed_out = true;
            match child.kill() {
                Ok(()) => break child.wait().map_err(MirageError::from)?,
                Err(error) => match child.try_wait().map_err(MirageError::from)? {
                    Some(status) => break status,
                    None => return Err(MirageError::from(error)),
                },
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !launch.drain_interval.is_zero() {
        std::thread::sleep(launch.drain_interval.min(Duration::from_secs(30)));
    }
    let capture = etw.stop_capture()?;
    Ok(ProfileResult {
        process_id,
        exit_code: status.code(),
        timed_out,
        elapsed: started.elapsed(),
        etw_events_lost: capture.metrics.events_lost,
        etw_buffers_lost: capture.metrics.realtime_buffers_lost,
        unknown_path_events: capture.unknown_paths,
        dropped_capture_events: capture.dropped_events,
        events: capture.events,
        version_label: launch.version_label.clone(),
        configuration_label: launch.configuration_label.clone(),
    })
}

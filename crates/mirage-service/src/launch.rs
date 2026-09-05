use mirage_types::{CapsuleId, GenerationId, MirageError};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Sealed,
    Balanced,
}
#[derive(Debug, Clone)]
pub struct LaunchReadiness {
    pub mounted_generation: GenerationId,
    pub requested_generation: GenerationId,
    pub admitted_capsule: Option<CapsuleId>,
    pub requested_capsule: Option<CapsuleId>,
    pub capsule_complete: bool,
    pub hard_set_complete: bool,
    pub update_or_recovery_active: bool,
    pub provider_healthy: bool,
    pub risk_millionths: u32,
}
pub struct LaunchPolicy;
impl LaunchPolicy {
    pub fn validate(mode: LaunchMode, r: &LaunchReadiness) -> Result<(), MirageError> {
        if r.mounted_generation != r.requested_generation {
            return Err(MirageError::repository_conflict(
                "requested launch generation is not mounted",
            ));
        }
        if r.update_or_recovery_active {
            return Err(MirageError::update_active(
                "update or recovery blocks launch",
            ));
        }
        match mode {
            LaunchMode::Sealed
                if r.requested_capsule.is_none()
                    || r.requested_capsule != r.admitted_capsule
                    || !r.capsule_complete =>
            {
                Err(MirageError::integrity_mismatch(
                    "sealed launch requires matching complete admitted capsule",
                ))
            }
            LaunchMode::Balanced if !r.hard_set_complete => Err(MirageError::cache_full(
                "balanced launch hard set is incomplete",
            )),
            LaunchMode::Balanced if !r.provider_healthy => Err(MirageError::backend_unavailable(
                "balanced launch requires a healthy provider",
            )),
            _ => Ok(()),
        }
    }
}
#[derive(Debug, Clone)]
pub struct NativeLaunch {
    pub game_root: PathBuf,
    pub launcher: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}
#[derive(Debug)]
pub struct LaunchedProcess {
    pub child: Child,
    pub root_pid: u32,
    pub launched_at: SystemTime,
}
pub fn launch_native(spec: &NativeLaunch) -> Result<LaunchedProcess, MirageError> {
    if spec.arguments.len() > 256
        || spec.environment.len() > 64
        || spec.environment.keys().any(|k| {
            k.is_empty()
                || k.len() > 128
                || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        || spec.environment.values().any(|v| v.len() > 32_767)
    {
        return Err(MirageError::invalid_argument(
            "native launch arguments or environment are outside bounds",
        ));
    }
    let root = spec.game_root.canonicalize().map_err(MirageError::from)?;
    let launcher = spec.launcher.canonicalize().map_err(MirageError::from)?;
    if !launcher.starts_with(&root) || !launcher.is_file() {
        return Err(MirageError::invalid_argument(
            "launcher is outside registered native root",
        ));
    }
    let mut command = Command::new(launcher);
    command
        .args(&spec.arguments)
        .current_dir(root)
        .env_clear()
        .envs(&spec.environment);
    let child = command.spawn().map_err(MirageError::from)?;
    let root_pid = child.id();
    Ok(LaunchedProcess {
        child,
        root_pid,
        launched_at: SystemTime::now(),
    })
}

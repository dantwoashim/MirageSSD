//! Process isolation and exact lifecycle state for filesystem hosts.
#![deny(unsafe_code)]

#[cfg(windows)]
#[allow(unsafe_code)]
mod authorization;
#[allow(unsafe_code)]
mod disk_space;
#[cfg(windows)]
#[allow(unsafe_code)]
mod ipc_server;

mod capacity;
mod control_plane;
mod conversion;
mod credential_broker;
mod launch;
mod mount_control;
mod native_session;
mod process_tree;
mod profile_session;
mod repository_actor;
mod restore_native;
mod rollback_point;
mod runtime;
mod supervisor;
#[cfg(windows)]
pub use authorization::{
    authenticated_named_pipe_client_principal, authenticated_named_pipe_client_sid,
};
pub use control_plane::ControlPlaneHandler;
pub use conversion::{ConversionAction, ConversionPhase, ConversionTransaction};
pub use credential_broker::{AccessCapability, CredentialBroker};
#[cfg(windows)]
pub use ipc_server::{RequestHandler, serve_one, serve_one_named, wake_server};
pub use launch::{LaunchMode, LaunchPolicy, LaunchReadiness, NativeLaunch, launch_native};
pub use mount_control::{MountControl, NativeMountControl};
pub use process_tree::{ProcessIdentity, ProcessTracker, TrackedRole};
pub use profile_session::{ProfileLaunch, ProfileResult, run_profile_session};
pub use repository_actor::{RepositoryActor, RepositorySnapshot};
pub use restore_native::{RestoreAction, RestorePhase, RestoreTransaction};
pub use rollback_point::{RollbackEntry, RollbackPoint};
pub use supervisor::{
    HostExit, HostId, HostSpec, HostState, Launcher, ManagedChild, StdLauncher, Supervisor,
};

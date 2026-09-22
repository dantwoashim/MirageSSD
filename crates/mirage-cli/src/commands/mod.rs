use clap::Subcommand;
use mirage_types::MirageError;

#[allow(unsafe_code)]
pub mod agent;
pub mod backend_login;
mod cache_check;
mod cache_verify;
mod config;
mod db_check;
mod device_backup;
mod device_drive;
mod disk;
mod drive_gate;
mod drive_live;
mod recovery;
mod repo_drive;
mod repo_import_local;
mod repo_local;
mod repo_scan;
mod service;
mod simulate;
mod version;
#[allow(unsafe_code)]
pub mod volume;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show service-owned repository, cache, backend, update, and readiness state.
    Status,
    /// Print build and schema versions.
    Version,
    /// Validate service configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect the durable control-plane database without modifying it.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Per-disk free-space floors enforced by evicting cloud-backed data.
    Disk {
        #[command(subcommand)]
        command: DiskCommand,
    },
    /// Pin a namespace path so its data is never evicted by reclaim.
    Pin {
        /// Repository id (hex) of the managed volume.
        repository_id: mirage_types::RepositoryId,
        /// Mount-relative path, e.g. `games/saves` or `/games/saves`.
        path: String,
    },
    /// Remove a pin previously set with `mirage pin`.
    Unpin {
        /// Repository id (hex) of the managed volume.
        repository_id: mirage_types::RepositoryId,
        /// Mount-relative path.
        path: String,
    },
    /// List pinned paths of a repository.
    Pins {
        /// Repository id (hex) of the managed volume.
        repository_id: mirage_types::RepositoryId,
    },
    /// Inspect or reconcile the local sparse cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Authenticate and inspect remote backends.
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    /// Repository import and integrity operations.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Capture or analyze access profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Run deterministic cache and session simulation.
    Simulate {
        #[arg(long)]
        trace: std::path::PathBuf,
        #[arg(long)]
        index: std::path::PathBuf,
        #[arg(long)]
        cache_pages: usize,
        #[arg(long)]
        latency_ms: u64,
        #[arg(long)]
        bandwidth_bytes_per_second: u64,
    },
    /// Sweep explicit simulator parameters and write a Gate A report.
    SimulateSweep {
        #[arg(long)]
        trace: std::path::PathBuf,
        #[arg(long)]
        index: std::path::PathBuf,
        #[arg(long, value_delimiter = ',')]
        page_bytes: Vec<u64>,
        #[arg(long, value_delimiter = ',')]
        cache_pages: Vec<usize>,
        #[arg(long, value_delimiter = ',')]
        latency_ms: Vec<u64>,
        #[arg(long)]
        bandwidth_bytes_per_second: u64,
        #[arg(long)]
        blocking_weight: u64,
        #[arg(long)]
        remote_weight: u64,
        #[arg(long)]
        cache_weight: u64,
        #[arg(long)]
        markdown: std::path::PathBuf,
    },
    /// Inspect and acquire truthful physical local-space promises.
    Capacity {
        #[command(subcommand)]
        command: CapacityCommand,
    },
    /// Materialize a verified Drive generation as an ordinary native NTFS tree.
    Native {
        #[command(subcommand)]
        command: NativeCommand,
    },
    /// Create or list managed Drive-backed volumes (first-run flow).
    Volume {
        #[command(subcommand)]
        command: VolumeCommand,
    },
    /// Mount a prepared repository.
    Mount {
        repository_id: mirage_types::RepositoryId,
        generation: mirage_types::GenerationId,
        /// Mount as an Explorer-visible read-only drive, for example M.
        #[arg(long)]
        drive_letter: Option<String>,
        /// Desktop OAuth JSON used to mint a Drive access token for a
        /// managed Drive-backed mount (enables on-demand page fetches).
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long, requires = "client_credentials")]
        token_store: Option<std::path::PathBuf>,
    },
    /// Per-user logon agent: remount managed volumes and refresh Drive
    /// tokens. Registered automatically on sign-in.
    #[command(hide = true)]
    Agent {
        /// Run one mount/refresh cycle and exit (for tests).
        #[arg(long, hide = true)]
        once: bool,
    },
    /// Unmount a repository.
    Unmount {
        repository_id: mirage_types::RepositoryId,
    },
    /// Plan or materialize sealed session capsules.
    Capsule {
        #[command(subcommand)]
        command: CapsuleCommand,
    },
    /// Launch through the admission controller.
    Launch {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        capsule_id: Option<mirage_types::CapsuleId>,
        /// Stop the launched process after this many seconds.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=21_600))]
        maximum_duration_seconds: Option<u64>,
    },
    /// Manage an exclusive game update transaction.
    Update {
        #[command(subcommand)]
        command: UpdateCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackendCommand {
    /// Authenticate Google Drive in the interactive user session.
    Login {
        #[arg(long)]
        client_id: Option<String>,
        /// Google Desktop OAuth JSON file. Its client secret is never logged or persisted.
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = 300)]
        timeout_seconds: u64,
    },
    /// Show encrypted Google Drive credential metadata without revealing tokens.
    Status {
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Delete the current user's encrypted Google Drive credential record.
    Logout {
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Refresh the protected token and verify the live Drive account and quota.
    VerifyLive {
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Push a fresh Drive bearer token to one mounted managed repository.
    SupplyToken {
        repository_id: mirage_types::RepositoryId,
        /// Desktop OAuth JSON used only to refresh a short-lived Drive access token.
        /// Defaults to the installed oauth-desktop.json.
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Refresh and push Drive bearer tokens to every mounted managed
    /// Drive-backed repository until Ctrl-C (access tokens expire ~1h).
    TokenAgent {
        /// Desktop OAuth JSON used only to refresh Drive access tokens.
        /// Defaults to the installed oauth-desktop.json.
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        /// Seconds between pushes; Drive access tokens live about an hour.
        #[arg(long, default_value_t = 2700)]
        interval_seconds: u64,
    },
    /// Run authenticated Drive Gates D and F with encrypted signed generations.
    GateDrive {
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        /// Empty or resumable state directory outside the source repository.
        #[arg(long)]
        work_directory: std::path::PathBuf,
    },
    /// Protect the Desktop OAuth client credential with current-user DPAPI for auto-mounting.
    AuthorizeDevice {
        #[arg(long)]
        drive_client_credentials: std::path::PathBuf,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Mount a persistent, writable Drive-backed volume through the local SSD cache.
    MountDevice {
        /// Rclone executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        rclone: Option<std::path::PathBuf>,
        /// Free Windows drive letter, for example N.
        #[arg(long, default_value = "N")]
        drive_letter: String,
        /// Dedicated Google Drive folder exposed by this volume.
        #[arg(long, default_value = "MirageSSD Storage")]
        remote_folder: String,
        /// Absolute local SSD cache directory.
        #[arg(long)]
        cache_dir: std::path::PathBuf,
        /// Absolute diagnostic log path. OAuth material is never logged.
        #[arg(long)]
        log_file: std::path::PathBuf,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        #[arg(long, default_value = "128Gi")]
        cache_max_size: String,
        #[arg(long, default_value = "24Gi")]
        cache_min_free_space: String,
        #[arg(long, default_value = "720h")]
        cache_max_age: String,
        #[arg(long, default_value = "128Mi")]
        read_ahead: String,
        #[arg(long, default_value = "16Mi")]
        read_chunk_size: String,
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=16))]
        read_chunk_streams: u8,
    },
    /// Bulk-copy a local file or directory into the Drive-backed device.
    IngestDevice {
        /// Rclone executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        rclone: Option<std::path::PathBuf>,
        /// Absolute local file or directory to import.
        #[arg(long)]
        source: std::path::PathBuf,
        /// Dedicated Google Drive folder backing the device.
        #[arg(long, default_value = "MirageSSD Storage")]
        remote_folder: String,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Create an encrypted, deduplicated whole-PC snapshot in Google Drive.
    BackupDevice {
        /// Restic executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        restic: Option<std::path::PathBuf>,
        /// Rclone executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        rclone: Option<std::path::PathBuf>,
        /// Absolute volume or directory to back up. Repeat for multiple sources.
        #[arg(long = "source", required = true)]
        sources: Vec<std::path::PathBuf>,
        /// Absolute reconstructable or transient path to exclude. Repeat as needed.
        #[arg(long = "exclude")]
        exclusions: Vec<std::path::PathBuf>,
        /// Drive folder containing the encrypted Restic repository.
        #[arg(long, default_value = "MirageSSD Storage/System Backup")]
        remote_folder: String,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        /// DPAPI-protected Restic repository key. Defaults outside the repository.
        #[arg(long)]
        key_store: Option<std::path::PathBuf>,
        /// Durable non-secret progress state JSON.
        #[arg(long)]
        state_file: Option<std::path::PathBuf>,
        /// Append-only Restic output log.
        #[arg(long)]
        log_file: Option<std::path::PathBuf>,
    },
    /// Plan or execute a verified restore from the encrypted PC backup.
    RestoreDevice {
        /// Restic executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        restic: Option<std::path::PathBuf>,
        /// Rclone executable. When omitted, MirageSSD locates the Winget installation.
        #[arg(long)]
        rclone: Option<std::path::PathBuf>,
        /// Snapshot ID or latest.
        #[arg(long, default_value = "latest")]
        snapshot: String,
        /// Absolute folder from the snapshot whose contents will be restored.
        #[arg(long)]
        include: std::path::PathBuf,
        /// Absolute local directory receiving the restored tree.
        #[arg(long)]
        target: std::path::PathBuf,
        /// Drive folder containing the encrypted Restic repository.
        #[arg(long, default_value = "MirageSSD Storage/System Backup")]
        remote_folder: String,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        /// DPAPI-protected Restic repository key.
        #[arg(long)]
        key_store: Option<std::path::PathBuf>,
        /// Append-only Restic output log.
        #[arg(long)]
        log_file: Option<std::path::PathBuf>,
        /// Check repository structure before planning or restoring.
        #[arg(long)]
        check_repository: bool,
        /// Write and verify restored files. Omit for a non-mutating plan.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Parse and validate a TOML service configuration.
    Validate { path: std::path::PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Run SQLite integrity and foreign-key checks through a read-only connection.
    Check { path: std::path::PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Plan or apply startup cache reconciliation.
    Check {
        #[arg(long)]
        db: std::path::PathBuf,
        #[arg(long)]
        arena: std::path::PathBuf,
        #[arg(long)]
        page_size: u64,
        #[arg(long)]
        slot_count: u32,
        #[arg(long)]
        dry_run: bool,
    },
    /// Hash selected resident pages and quarantine corrupt clean pages.
    Verify {
        #[arg(long)]
        db: std::path::PathBuf,
        #[arg(long)]
        arena: std::path::PathBuf,
        #[arg(long)]
        page_size: u64,
        #[arg(long)]
        slot_count: u32,
        #[arg(long)]
        max_pages: Option<usize>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CapacityCommand {
    /// Preview immediately free and safely reclaimable physical bytes without changing residency.
    Plan {
        repository_id: mirage_types::RepositoryId,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
        /// Desktop OAuth JSON used to verify Drive recovery objects before reporting reclaimable bytes.
        #[arg(long)]
        drive_client_credentials: Option<std::path::PathBuf>,
        #[arg(long, requires = "drive_client_credentials")]
        drive_token_store: Option<std::path::PathBuf>,
    },
    /// Revalidate and reclaim clean cache pages, then create a durable physical Space Lease.
    Acquire {
        repository_id: mirage_types::RepositoryId,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(30..=86_400))]
        lifetime_seconds: u64,
        /// Desktop OAuth JSON used only to refresh a short-lived Drive access token.
        #[arg(long)]
        drive_client_credentials: Option<std::path::PathBuf>,
        #[arg(long, requires = "drive_client_credentials")]
        drive_token_store: Option<std::path::PathBuf>,
    },
    /// Show the target volume's active promises and optionally one lease.
    Status {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        lease_id: Option<mirage_types::SpaceLeaseId>,
    },
    /// Mark a ready lease as actively consumed by a guarded operation.
    Consume {
        repository_id: mirage_types::RepositoryId,
        lease_id: mirage_types::SpaceLeaseId,
    },
    /// Release a durable Space Lease after the guarded write or install finishes.
    Release {
        repository_id: mirage_types::RepositoryId,
        lease_id: mirage_types::SpaceLeaseId,
    },
    /// Acquire capacity, run a native program, and release the lease on every normal exit path.
    Run {
        repository_id: mirage_types::RepositoryId,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
        #[arg(long, default_value_t = 21_600, value_parser = clap::value_parser!(u64).range(30..=86_400))]
        lifetime_seconds: u64,
        #[arg(long)]
        drive_client_credentials: Option<std::path::PathBuf>,
        #[arg(long, requires = "drive_client_credentials")]
        drive_token_store: Option<std::path::PathBuf>,
        #[arg(long)]
        program: std::path::PathBuf,
        #[arg(last = true, allow_hyphen_values = true)]
        arguments: Vec<std::ffi::OsString>,
    },
}

#[derive(Debug, Subcommand)]
pub enum NativeCommand {
    /// Reserve local bytes, verify every Drive-backed page, and atomically publish ordinary files.
    Activate {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        drive_client_credentials: std::path::PathBuf,
        #[arg(long)]
        drive_token_store: Option<std::path::PathBuf>,
    },
    /// Show durable native activation and recovery state.
    Status {
        repository_id: mirage_types::RepositoryId,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// Register a verified local import with the service and bind it to your Windows SID.
    Register {
        #[arg(long)]
        import: std::path::PathBuf,
        #[arg(long)]
        native_root: std::path::PathBuf,
        /// Reviewed immutable subtree relative to the native game root.
        #[arg(long, default_value = ".")]
        mount_subtree: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        launcher: std::path::PathBuf,
        #[arg(long = "arg")]
        arguments: Vec<String>,
        #[arg(long, default_value = "unknown")]
        version_label: String,
        #[arg(long, default_value = "default")]
        configuration_label: String,
        #[arg(long, default_value_t = 8_589_934_592)]
        cache_bytes: u64,
    },
    /// Adopt a legacy SYSTEM-owned repository as the current elevated Windows user.
    Adopt {
        repository_id: mirage_types::RepositoryId,
    },
    /// Plan a reversible subtree conversion, or apply it with explicit confirmation.
    Convert {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        apply: bool,
    },
    /// Plan native restoration, or apply it with explicit confirmation.
    RestoreNative {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        apply: bool,
    },
    /// Scan a source tree without following reparse points or changing source bytes.
    Scan {
        root: std::path::PathBuf,
        #[arg(long)]
        report: std::path::PathBuf,
        /// Additional reviewed immutable asset extensions (without a leading dot).
        #[arg(long = "virtual-extension", value_delimiter = ',')]
        virtual_extensions: Vec<String>,
        #[arg(long, default_value_t = 1_048_576)]
        minimum_virtual_asset_bytes: u64,
    },
    /// Import a native installation into an immutable repository.
    Import {
        /// Required safety acknowledgement: build local packs only; never upload or delete source.
        #[arg(long)]
        local_only: bool,
        #[arg(long)]
        source: std::path::PathBuf,
        #[arg(long)]
        output: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long, default_value = "0")]
        generation: mirage_types::GenerationId,
        #[arg(long, default_value_t = 1_048_576)]
        page_size: u32,
        #[arg(long, default_value_t = 536_870_912)]
        pack_target: u64,
        /// Additional reviewed immutable asset extensions (without a leading dot).
        #[arg(long = "virtual-extension", value_delimiter = ',')]
        virtual_extensions: Vec<String>,
        #[arg(long, default_value_t = 1_048_576)]
        minimum_virtual_asset_bytes: u64,
        /// Explicit compatibility mode. Normal imports encrypt every virtual page.
        #[arg(long)]
        unencrypted: bool,
        /// Pack every regular file regardless of size or extension so every
        /// byte is fetchable on a managed Drive-backed volume.
        #[arg(long)]
        pack_all: bool,
    },
    /// Publish a completed local import to a local immutable backend.
    CommitLocal {
        #[arg(long)]
        import: std::path::PathBuf,
        #[arg(long)]
        backend_root: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        key_id_hex: String,
        #[arg(long)]
        test_key_hex: String,
    },
    /// Publish an encrypted local import as a signed immutable Drive generation.
    PublishDrive {
        #[arg(long)]
        import: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
        #[arg(long)]
        key_id_hex: String,
        #[arg(long)]
        test_key_hex: String,
    },
    /// Use the verified Drive publication as this repository's materialization origin.
    UseDrive {
        repository_id: mirage_types::RepositoryId,
    },
    /// Return this repository to its verified local-pack materialization origin.
    UseLocal {
        repository_id: mirage_types::RepositoryId,
    },
    /// Select the writable managed volume mode, or return to legacy read-only mounts.
    SetVolumeMode {
        repository_id: mirage_types::RepositoryId,
        #[arg(long, required_unless_present = "legacy", conflicts_with = "legacy")]
        managed: bool,
        #[arg(long)]
        legacy: bool,
    },
    /// Verify repository commits and referenced content.
    Verify {
        #[arg(long)]
        backend_root: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        key_id_hex: String,
        #[arg(long)]
        test_key_hex: String,
        #[arg(long, default_value = "metadata")]
        level: String,
    },
    /// Reconstruct virtual files without overwriting existing paths.
    Extract {
        #[arg(long)]
        backend_root: std::path::PathBuf,
        #[arg(long)]
        destination: std::path::PathBuf,
        #[arg(long)]
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        key_id_hex: String,
        #[arg(long)]
        test_key_hex: String,
        /// DPAPI-protected repository content key created during import.
        #[arg(long)]
        repository_key: Option<std::path::PathBuf>,
        /// Recovery envelope supplying the content key instead of --repository-key.
        #[arg(long, requires = "secret_file", conflicts_with = "repository_key")]
        envelope: Option<std::path::PathBuf>,
        /// File containing the recovery secret for --envelope.
        #[arg(long)]
        secret_file: Option<std::path::PathBuf>,
    },
    /// Repair recoverable local repository state.
    Repair {
        repository_id: mirage_types::RepositoryId,
    },
    /// Export, verify, or restore a portable recovery envelope.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum DiskCommand {
    /// Keep at least this many bytes free on the volume (e.g. `D: 150GiB`).
    SetFloor {
        /// Drive root such as `D:` or `D:\`.
        volume_root: String,
        /// Free-space floor, e.g. `150GiB` or plain bytes.
        floor: String,
        /// Extra headroom to restore past the floor (default: max(1 GiB, 5% of floor)).
        #[arg(long)]
        hysteresis: Option<String>,
    },
    /// Remove the free-space floor from a volume.
    ClearFloor {
        /// Drive root such as `D:` or `D:\`.
        volume_root: String,
    },
    /// Show free space, floor state, and the last reclaim run per volume.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    /// Create an empty managed Drive-backed volume and mount it.
    Create {
        /// Explorer drive letter (default: first free letter from M).
        #[arg(long)]
        letter: Option<String>,
        /// Display name for the new volume.
        #[arg(long, default_value = "MirageSSD")]
        name: String,
        /// Local SSD budget for staged writes, e.g. `64GiB`
        /// (default: min(25% of the freest disk, 64 GiB)).
        #[arg(long)]
        budget: Option<String>,
        /// Keep at least this many bytes free on the state disk
        /// (default: max(10% of that disk, 20 GiB)).
        #[arg(long)]
        floor: Option<String>,
        /// Desktop OAuth JSON; defaults to the installed oauth-desktop.json.
        #[arg(long)]
        client_credentials: Option<std::path::PathBuf>,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// List this user's managed Drive-backed volumes.
    List,
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    /// Export an encrypted recovery envelope containing the repository content key.
    Export {
        /// Import directory holding the DPAPI-protected repository key record.
        #[arg(long)]
        import: std::path::PathBuf,
        /// New file receiving the encrypted envelope. Refuses to overwrite.
        #[arg(long)]
        envelope: std::path::PathBuf,
        /// File containing the recovery secret. It is never logged or persisted elsewhere.
        #[arg(long)]
        secret_file: std::path::PathBuf,
        /// Optional DPAPI-protected signer record to include for authority recovery.
        #[arg(long)]
        signer_store: Option<std::path::PathBuf>,
    },
    /// Verify a recovery envelope decrypts to this repository's content key.
    Verify {
        #[arg(long)]
        envelope: std::path::PathBuf,
        /// File containing the recovery secret.
        #[arg(long)]
        secret_file: std::path::PathBuf,
        /// Import directory whose key record the envelope must match. When supplied,
        /// a durable verification record is written for the reclamation gate.
        #[arg(long)]
        import: Option<std::path::PathBuf>,
        /// Expected repository when --import is not available.
        #[arg(long)]
        repository_id: Option<mirage_types::RepositoryId>,
    },
    /// Restore envelope secrets into a fresh DPAPI-protected key store on this machine.
    Import {
        #[arg(long)]
        envelope: std::path::PathBuf,
        /// File containing the recovery secret.
        #[arg(long)]
        secret_file: std::path::PathBuf,
        /// Directory receiving the restored repository key record.
        #[arg(long)]
        destination: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Update the safe repository-relative launcher and profile labels.
    Configure {
        repository_id: mirage_types::RepositoryId,
        #[arg(long)]
        launcher: std::path::PathBuf,
        #[arg(long = "arg")]
        arguments: Vec<String>,
        #[arg(long)]
        version_label: String,
        #[arg(long)]
        configuration_label: String,
    },
    /// Capture a first-touch and ETW profile.
    Capture {
        repository_id: mirage_types::RepositoryId,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..=21_600))]
        maximum_duration_seconds: u32,
    },
    /// Analyze captured profiles.
    Analyze {
        repository_id: mirage_types::RepositoryId,
    },
}

#[derive(Debug, Subcommand)]
pub enum CapsuleCommand {
    /// Plan a session capsule without allocating cache slots.
    Plan {
        repository_id: mirage_types::RepositoryId,
        /// Include every repository page for a fully offline Explorer volume.
        #[arg(long)]
        full_volume: bool,
    },
    /// Materialize, verify, reserve, and pin a planned capsule.
    Materialize {
        repository_id: mirage_types::RepositoryId,
        capsule_id: mirage_types::CapsuleId,
        /// Desktop OAuth client JSON used only to refresh a short-lived Drive access token.
        #[arg(long)]
        drive_client_credentials: Option<std::path::PathBuf>,
        #[arg(long, requires = "drive_client_credentials")]
        drive_token_store: Option<std::path::PathBuf>,
    },
    /// Atomically lease and admit a fully materialized capsule.
    Admit {
        repository_id: mirage_types::RepositoryId,
        capsule_id: mirage_types::CapsuleId,
    },
}

#[derive(Debug, Subcommand)]
pub enum UpdateCommand {
    /// Begin an exclusive durable update journal.
    Begin {
        repository_id: mirage_types::RepositoryId,
    },
    /// Show current update and recovery state.
    Status {
        repository_id: mirage_types::RepositoryId,
    },
    /// Commit a verified update generation.
    Commit {
        repository_id: mirage_types::RepositoryId,
    },
    /// Roll back the active update journal.
    Rollback {
        repository_id: mirage_types::RepositoryId,
    },
}

pub fn dispatch(command: Command, json: bool) -> Result<(), MirageError> {
    match command {
        Command::Status => service::run(mirage_ipc::Command::Status, json),
        Command::Version => version::run(json),
        Command::Config {
            command: ConfigCommand::Validate { path },
        } => config::validate_path(&path, json),
        Command::Db {
            command: DbCommand::Check { path },
        } => db_check::run(&path, json),
        Command::Cache { command } => match command {
            CacheCommand::Check {
                db,
                arena,
                page_size,
                slot_count,
                dry_run,
            } => cache_check::run(&db, &arena, page_size, slot_count, dry_run, json),
            CacheCommand::Verify {
                db,
                arena,
                page_size,
                slot_count,
                max_pages,
            } => cache_verify::run(&db, &arena, page_size, slot_count, max_pages, json),
        },
        Command::Backend { command } => match command {
            BackendCommand::Login {
                client_id,
                client_credentials,
                token_store,
                timeout_seconds,
            } => backend_login::login(
                client_id.as_deref(),
                client_credentials.as_deref(),
                token_store.as_deref(),
                timeout_seconds,
                json,
            ),
            BackendCommand::Status { token_store } => {
                backend_login::status(token_store.as_deref(), json)
            }
            BackendCommand::Logout { token_store } => {
                backend_login::logout(token_store.as_deref(), json)
            }
            BackendCommand::VerifyLive {
                client_credentials,
                token_store,
            } => drive_live::verify(
                &backend_login::oauth_client_credentials(client_credentials)?,
                token_store.as_deref(),
                json,
            ),
            BackendCommand::SupplyToken {
                repository_id,
                client_credentials,
                token_store,
            } => {
                let token = capacity_drive_token(
                    Some(backend_login::oauth_client_credentials(client_credentials)?).as_deref(),
                    token_store.as_deref(),
                )?
                .ok_or_else(|| MirageError::invalid_argument("--client-credentials is required"))?;
                service::run(
                    mirage_ipc::Command::DriveTokenSupply {
                        repository_id,
                        drive_access_token: token,
                    },
                    json,
                )
            }
            BackendCommand::TokenAgent {
                client_credentials,
                token_store,
                interval_seconds,
            } => token_agent(
                &backend_login::oauth_client_credentials(client_credentials)?,
                token_store.as_deref(),
                interval_seconds,
                json,
            ),
            BackendCommand::GateDrive {
                client_credentials,
                token_store,
                work_directory,
            } => drive_gate::run(
                &backend_login::oauth_client_credentials(client_credentials)?,
                token_store.as_deref(),
                &work_directory,
                json,
            ),
            BackendCommand::AuthorizeDevice {
                drive_client_credentials,
                token_store,
            } => device_drive::authorize(&drive_client_credentials, token_store.as_deref(), json),
            BackendCommand::MountDevice {
                rclone,
                drive_letter,
                remote_folder,
                cache_dir,
                log_file,
                token_store,
                cache_max_size,
                cache_min_free_space,
                cache_max_age,
                read_ahead,
                read_chunk_size,
                read_chunk_streams,
            } => device_drive::mount(device_drive::MountOptions {
                rclone: rclone.as_deref(),
                drive_letter: &drive_letter,
                remote_folder: &remote_folder,
                cache_dir: &cache_dir,
                log_file: &log_file,
                token_store: token_store.as_deref(),
                cache_max_size: &cache_max_size,
                cache_min_free_space: &cache_min_free_space,
                cache_max_age: &cache_max_age,
                read_ahead: &read_ahead,
                read_chunk_size: &read_chunk_size,
                read_chunk_streams,
                json,
            }),
            BackendCommand::IngestDevice {
                rclone,
                source,
                remote_folder,
                token_store,
            } => device_drive::ingest(device_drive::IngestOptions {
                rclone: rclone.as_deref(),
                source: &source,
                remote_folder: &remote_folder,
                token_store: token_store.as_deref(),
                json,
            }),
            BackendCommand::BackupDevice {
                restic,
                rclone,
                sources,
                exclusions,
                remote_folder,
                token_store,
                key_store,
                state_file,
                log_file,
            } => device_backup::backup(device_backup::BackupOptions {
                restic: restic.as_deref(),
                rclone: rclone.as_deref(),
                sources: &sources,
                exclusions: &exclusions,
                remote_folder: &remote_folder,
                token_store: token_store.as_deref(),
                key_store: key_store.as_deref(),
                state_file: state_file.as_deref(),
                log_file: log_file.as_deref(),
                json,
            }),
            BackendCommand::RestoreDevice {
                restic,
                rclone,
                snapshot,
                include,
                target,
                remote_folder,
                token_store,
                key_store,
                log_file,
                check_repository,
                apply,
            } => device_backup::restore(device_backup::RestoreOptions {
                restic: restic.as_deref(),
                rclone: rclone.as_deref(),
                snapshot: &snapshot,
                include: &include,
                target: &target,
                remote_folder: &remote_folder,
                token_store: token_store.as_deref(),
                key_store: key_store.as_deref(),
                log_file: log_file.as_deref(),
                check_repository,
                apply,
                json,
            }),
        },
        Command::Repo { command } => match command {
            RepoCommand::Register {
                import,
                native_root,
                mount_subtree,
                repository_id,
                display_name,
                launcher,
                arguments,
                version_label,
                configuration_label,
                cache_bytes,
            } => service::run(
                mirage_ipc::Command::RepositoryRegister {
                    repository_id,
                    display_name,
                    native_root,
                    mount_subtree,
                    import_root: import,
                    launcher_relative: launcher,
                    arguments,
                    version_label,
                    configuration_label,
                    cache_bytes,
                },
                json,
            ),
            RepoCommand::Adopt { repository_id } => {
                service::run(mirage_ipc::Command::RepositoryAdopt { repository_id }, json)
            }
            RepoCommand::Convert {
                repository_id,
                apply,
            } => service::run(
                mirage_ipc::Command::RepositoryConvert {
                    repository_id,
                    apply,
                },
                json,
            ),
            RepoCommand::RestoreNative {
                repository_id,
                apply,
            } => service::run(
                mirage_ipc::Command::RepositoryRestoreNative {
                    repository_id,
                    apply,
                },
                json,
            ),
            RepoCommand::Scan {
                root,
                report,
                virtual_extensions,
                minimum_virtual_asset_bytes,
            } => repo_scan::run(
                &root,
                &report,
                &virtual_extensions,
                minimum_virtual_asset_bytes,
                json,
            ),
            RepoCommand::Import {
                local_only,
                source,
                output,
                repository_id,
                generation,
                page_size,
                pack_target,
                virtual_extensions,
                minimum_virtual_asset_bytes,
                unencrypted,
                pack_all,
            } => repo_import_local::run(
                local_only,
                &source,
                &output,
                repository_id,
                generation,
                page_size,
                pack_target,
                &virtual_extensions,
                minimum_virtual_asset_bytes,
                unencrypted,
                pack_all,
                false,
                json,
            ),
            RepoCommand::CommitLocal {
                import,
                backend_root,
                repository_id,
                key_id_hex,
                test_key_hex,
            } => repo_local::commit_local(
                &import,
                &backend_root,
                repository_id,
                &key_id_hex,
                &test_key_hex,
                json,
            ),
            RepoCommand::PublishDrive {
                import,
                repository_id,
                client_credentials,
                token_store,
                key_id_hex,
                test_key_hex,
            } => repo_drive::publish(
                &import,
                repository_id,
                &backend_login::oauth_client_credentials(client_credentials)?,
                token_store.as_deref(),
                &key_id_hex,
                &test_key_hex,
                json,
            ),
            RepoCommand::UseDrive { repository_id } => service::run(
                mirage_ipc::Command::RepositorySetDriveOrigin {
                    repository_id,
                    drive: true,
                },
                json,
            ),
            RepoCommand::UseLocal { repository_id } => service::run(
                mirage_ipc::Command::RepositorySetDriveOrigin {
                    repository_id,
                    drive: false,
                },
                json,
            ),
            RepoCommand::SetVolumeMode {
                repository_id,
                managed,
                ..
            } => service::run(
                mirage_ipc::Command::RepositorySetVolumeMode {
                    repository_id,
                    managed,
                },
                json,
            ),
            RepoCommand::Verify {
                backend_root,
                repository_id,
                key_id_hex,
                test_key_hex,
                level,
            } => repo_local::verify(
                &backend_root,
                repository_id,
                &key_id_hex,
                &test_key_hex,
                &level,
                json,
            ),
            RepoCommand::Extract {
                backend_root,
                destination,
                repository_id,
                key_id_hex,
                test_key_hex,
                repository_key,
                envelope,
                secret_file,
            } => repo_local::extract(
                &backend_root,
                &destination,
                repository_id,
                &key_id_hex,
                &test_key_hex,
                repo_local::ExtractKeySource {
                    repository_key: repository_key.as_deref(),
                    envelope: envelope.as_deref(),
                    secret_file: secret_file.as_deref(),
                },
                json,
            ),
            RepoCommand::Repair { repository_id } => {
                service::run(mirage_ipc::Command::Repair { repository_id }, json)
            }
            RepoCommand::Recovery { command } => match command {
                RecoveryCommand::Export {
                    import,
                    envelope,
                    secret_file,
                    signer_store,
                } => recovery::export(
                    &import,
                    &envelope,
                    &secret_file,
                    signer_store.as_deref(),
                    json,
                ),
                RecoveryCommand::Verify {
                    envelope,
                    secret_file,
                    import,
                    repository_id,
                } => recovery::verify(
                    &envelope,
                    &secret_file,
                    import.as_deref(),
                    repository_id,
                    json,
                ),
                RecoveryCommand::Import {
                    envelope,
                    secret_file,
                    destination,
                } => recovery::import(&envelope, &secret_file, &destination, json),
            },
        },
        Command::Disk { command } => match command {
            DiskCommand::SetFloor {
                volume_root,
                floor,
                hysteresis,
            } => service::run(
                mirage_ipc::Command::DiskFloorSet {
                    volume_root,
                    floor_bytes: disk::parse_byte_size(&floor)?,
                    hysteresis_bytes: hysteresis
                        .map(|value| disk::parse_byte_size(&value))
                        .transpose()?,
                },
                json,
            ),
            DiskCommand::ClearFloor { volume_root } => {
                service::run(mirage_ipc::Command::DiskFloorClear { volume_root }, json)
            }
            DiskCommand::Status => service::run(mirage_ipc::Command::DiskStatus, json),
        },
        Command::Pin {
            repository_id,
            path,
        } => service::run(
            mirage_ipc::Command::NamespacePin {
                repository_id,
                path,
            },
            json,
        ),
        Command::Unpin {
            repository_id,
            path,
        } => service::run(
            mirage_ipc::Command::NamespaceUnpin {
                repository_id,
                path,
            },
            json,
        ),
        Command::Pins { repository_id } => {
            service::run(mirage_ipc::Command::NamespacePins { repository_id }, json)
        }
        Command::Profile { command } => match command {
            ProfileCommand::Configure {
                repository_id,
                launcher,
                arguments,
                version_label,
                configuration_label,
            } => service::run(
                mirage_ipc::Command::ProfileConfigure {
                    repository_id,
                    launcher_relative: launcher,
                    arguments,
                    version_label,
                    configuration_label,
                },
                json,
            ),
            ProfileCommand::Capture {
                repository_id,
                maximum_duration_seconds,
            } => service::run(
                mirage_ipc::Command::Profile {
                    repository_id,
                    maximum_duration_seconds,
                },
                json,
            ),
            ProfileCommand::Analyze { repository_id } => {
                service::run(mirage_ipc::Command::Simulate { repository_id }, json)
            }
        },
        Command::Simulate {
            trace,
            index,
            cache_pages,
            latency_ms,
            bandwidth_bytes_per_second,
        } => simulate::run(
            &trace,
            &index,
            cache_pages,
            latency_ms,
            bandwidth_bytes_per_second,
            json,
        ),
        Command::SimulateSweep {
            trace,
            index,
            page_bytes,
            cache_pages,
            latency_ms,
            bandwidth_bytes_per_second,
            blocking_weight,
            remote_weight,
            cache_weight,
            markdown,
        } => simulate::sweep(simulate::SweepArgs {
            trace: &trace,
            index: &index,
            page_bytes,
            cache_pages,
            latency_ms,
            bandwidth_bytes_per_second,
            blocking_weight,
            remote_weight,
            cache_weight,
            markdown: &markdown,
            json,
        }),
        Command::Capacity { command } => match command {
            CapacityCommand::Plan {
                repository_id,
                bytes,
                drive_client_credentials,
                drive_token_store,
            } => service::run(
                mirage_ipc::Command::CapacityPlan {
                    repository_id,
                    requested_bytes: bytes,
                    drive_access_token: capacity_drive_token(
                        drive_client_credentials.as_deref(),
                        drive_token_store.as_deref(),
                    )?,
                },
                json,
            ),
            CapacityCommand::Acquire {
                repository_id,
                bytes,
                lifetime_seconds,
                drive_client_credentials,
                drive_token_store,
            } => service::run(
                mirage_ipc::Command::CapacityAcquire {
                    repository_id,
                    requested_bytes: bytes,
                    lifetime_seconds,
                    drive_access_token: capacity_drive_token(
                        drive_client_credentials.as_deref(),
                        drive_token_store.as_deref(),
                    )?,
                },
                json,
            ),
            CapacityCommand::Status {
                repository_id,
                lease_id,
            } => service::run(
                mirage_ipc::Command::CapacityStatus {
                    repository_id,
                    lease_id,
                },
                json,
            ),
            CapacityCommand::Consume {
                repository_id,
                lease_id,
            } => service::run(
                mirage_ipc::Command::CapacityConsume {
                    repository_id,
                    lease_id,
                },
                json,
            ),
            CapacityCommand::Release {
                repository_id,
                lease_id,
            } => service::run(
                mirage_ipc::Command::CapacityRelease {
                    repository_id,
                    lease_id,
                },
                json,
            ),
            CapacityCommand::Run {
                repository_id,
                bytes,
                lifetime_seconds,
                drive_client_credentials,
                drive_token_store,
                program,
                arguments,
            } => run_with_capacity(CapacityRun {
                repository_id,
                bytes,
                lifetime_seconds,
                drive_client_credentials: drive_client_credentials.as_deref(),
                drive_token_store: drive_token_store.as_deref(),
                program: &program,
                arguments: &arguments,
                json,
            }),
        },
        Command::Native { command } => match command {
            NativeCommand::Activate {
                repository_id,
                drive_client_credentials,
                drive_token_store,
            } => {
                let token = capacity_drive_token(
                    Some(&drive_client_credentials),
                    drive_token_store.as_deref(),
                )?
                .ok_or_else(|| {
                    MirageError::backend_unauthenticated(
                        "native activation requires authenticated Drive credentials",
                    )
                })?;
                service::run(
                    mirage_ipc::Command::NativeActivate {
                        repository_id,
                        drive_access_token: Some(token),
                    },
                    json,
                )
            }
            NativeCommand::Status { repository_id } => {
                service::run(mirage_ipc::Command::NativeStatus { repository_id }, json)
            }
        },
        Command::Mount {
            repository_id,
            generation,
            drive_letter,
            client_credentials,
            token_store,
        } => service::run(
            mirage_ipc::Command::Mount {
                repository_id,
                generation,
                drive_letter,
                drive_access_token: capacity_drive_token(
                    client_credentials.as_deref(),
                    token_store.as_deref(),
                )?,
            },
            json,
        ),
        Command::Agent { once } => agent::run(once),
        Command::Unmount { repository_id } => {
            service::run(mirage_ipc::Command::Unmount { repository_id }, json)
        }
        Command::Volume { command } => match command {
            VolumeCommand::Create {
                letter,
                name,
                budget,
                floor,
                client_credentials,
                token_store,
            } => {
                let credentials = backend_login::oauth_client_credentials(client_credentials)?;
                let letter = match letter {
                    Some(letter) => letter,
                    None => volume::first_free_letter()?.to_string(),
                };
                let budget_bytes = match budget {
                    Some(value) => disk::parse_byte_size(&value)?,
                    None => volume::default_budget_bytes()?,
                };
                let floor_bytes = match floor {
                    Some(value) => Some(disk::parse_byte_size(&value)?),
                    None => Some(volume::default_floor_bytes(&volume::state_volume()?)),
                };
                let created = volume::create(
                    &volume::VolumeSpec {
                        name,
                        drive_letter: letter,
                        budget_bytes,
                        floor_bytes,
                    },
                    &credentials,
                    token_store.as_deref(),
                    &mut |step| {
                        if !json {
                            eprintln!("{step}...");
                        }
                    },
                    None,
                )?;
                service::emit(
                    serde_json::json!({
                        "repository_id": created.repository_id.to_string(),
                        "name": created.name,
                        "drive_letter": created.drive_letter,
                        "budget_bytes": created.budget_bytes,
                        "floor_bytes": created.floor_bytes,
                        "account_id": created.account_id,
                        "mount_point": created.mount_point,
                        "state": created.state,
                    }),
                    json,
                )
            }
            VolumeCommand::List => {
                service::emit(serde_json::json!({"volumes": volume::list(None)?}), json)
            }
        },
        Command::Capsule { command } => match command {
            CapsuleCommand::Plan {
                repository_id,
                full_volume,
            } => service::run(
                mirage_ipc::Command::Plan {
                    repository_id,
                    full_volume,
                },
                json,
            ),
            CapsuleCommand::Materialize {
                repository_id,
                capsule_id,
                drive_client_credentials,
                drive_token_store,
            } => {
                let (drive_access_token, drive_quota) = match drive_client_credentials.as_deref() {
                    Some(credentials) => {
                        let session =
                            drive_live::connect(credentials, drive_token_store.as_deref())?;
                        let quota = mirage_ipc::DriveQuotaSnapshot {
                            limit_bytes: session.quota.limit,
                            usage_bytes: session.quota.usage,
                        };
                        (
                            Some(mirage_ipc::SensitiveString::new(
                                session.access_token.as_str().to_owned(),
                            )?),
                            Some(quota),
                        )
                    }
                    None => (None, None),
                };
                service::run(
                    mirage_ipc::Command::Materialize {
                        repository_id,
                        capsule_id,
                        drive_access_token,
                        drive_quota,
                    },
                    json,
                )
            }
            CapsuleCommand::Admit {
                repository_id,
                capsule_id,
            } => service::run(
                mirage_ipc::Command::Admit {
                    repository_id,
                    capsule_id,
                },
                json,
            ),
        },
        Command::Launch {
            repository_id,
            capsule_id,
            maximum_duration_seconds,
        } => service::run(
            mirage_ipc::Command::Launch {
                repository_id,
                capsule_id,
                maximum_duration_seconds,
            },
            json,
        ),
        Command::Update { command } => match command {
            UpdateCommand::Begin { repository_id } => {
                service::run(mirage_ipc::Command::UpdateBegin { repository_id }, json)
            }
            UpdateCommand::Status { repository_id } => {
                service::run(mirage_ipc::Command::UpdateStatus { repository_id }, json)
            }
            UpdateCommand::Commit { repository_id } => {
                service::run(mirage_ipc::Command::UpdateCommit { repository_id }, json)
            }
            UpdateCommand::Rollback { repository_id } => {
                service::run(mirage_ipc::Command::UpdateRollback { repository_id }, json)
            }
        },
    }
}

fn capacity_drive_token(
    client_credentials: Option<&std::path::Path>,
    token_store: Option<&std::path::Path>,
) -> Result<Option<mirage_ipc::SensitiveString>, MirageError> {
    let credentials = client_credentials
        .map(std::path::Path::to_path_buf)
        .or_else(backend_login::default_client_credentials);
    credentials
        .map(|credentials| {
            let session = drive_live::connect(&credentials, token_store)?;
            mirage_ipc::SensitiveString::new(session.access_token.as_str().to_owned())
        })
        .transpose()
}

struct CapacityRun<'a> {
    repository_id: mirage_types::RepositoryId,
    bytes: u64,
    lifetime_seconds: u64,
    drive_client_credentials: Option<&'a std::path::Path>,
    drive_token_store: Option<&'a std::path::Path>,
    program: &'a std::path::Path,
    arguments: &'a [std::ffi::OsString],
    json: bool,
}

fn run_with_capacity(spec: CapacityRun<'_>) -> Result<(), MirageError> {
    let acquired = service::request_json(mirage_ipc::Command::CapacityAcquire {
        repository_id: spec.repository_id,
        requested_bytes: spec.bytes,
        lifetime_seconds: spec.lifetime_seconds,
        drive_access_token: capacity_drive_token(
            spec.drive_client_credentials,
            spec.drive_token_store,
        )?,
    })?;
    let lease_id = acquired["lease_id"]
        .as_str()
        .ok_or_else(|| MirageError::integrity_mismatch("capacity response omitted lease ID"))?
        .parse::<mirage_types::SpaceLeaseId>()?;

    if let Err(error) = service::request_json(mirage_ipc::Command::CapacityConsume {
        repository_id: spec.repository_id,
        lease_id,
    }) {
        let _ = service::request_json(mirage_ipc::Command::CapacityRelease {
            repository_id: spec.repository_id,
            lease_id,
        });
        return Err(error);
    }

    let process = std::process::Command::new(spec.program)
        .args(spec.arguments)
        .status()
        .map_err(MirageError::from);
    let released = service::request_json(mirage_ipc::Command::CapacityRelease {
        repository_id: spec.repository_id,
        lease_id,
    });

    let status = match (process, released) {
        (Err(process_error), Ok(_)) => return Err(process_error),
        (Ok(_), Err(release_error)) => return Err(release_error),
        (Err(process_error), Err(release_error)) => {
            return Err(MirageError::repository_conflict(format!(
                "guarded process failed ({process_error}) and Space Lease release failed ({release_error})"
            )));
        }
        (Ok(status), Ok(_)) => status,
    };
    if !status.success() {
        return Err(MirageError::repository_conflict(format!(
            "guarded process exited unsuccessfully with {}",
            status
                .code()
                .map_or_else(|| "no exit code".to_owned(), |code| code.to_string())
        )));
    }
    service::emit(
        serde_json::json!({
            "guarded": true,
            "repository_id": spec.repository_id.to_string(),
            "lease_id": lease_id.to_string(),
            "requested_bytes": spec.bytes,
            "exit_code": status.code(),
            "released": true
        }),
        spec.json,
    )
}

/// Refreshes and pushes a Drive bearer token to every mounted managed
/// Drive-backed repository, then sleeps `interval_seconds` and repeats until
/// Ctrl-C. One line is logged per push; the token itself is never printed.
fn token_agent(
    client_credentials: &std::path::Path,
    token_store: Option<&std::path::Path>,
    interval_seconds: u64,
    json: bool,
) -> Result<(), MirageError> {
    loop {
        let status = service::request_json(mirage_ipc::Command::Status)?;
        let repositories = status["repositories"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut supplied = 0_u32;
        for repository in repositories {
            if repository["state"].as_str() != Some("ready_mounted") {
                continue;
            }
            let Some(id_text) = repository["repository_id"].as_str() else {
                continue;
            };
            let Ok(repository_id) = id_text.parse::<mirage_types::RepositoryId>() else {
                continue;
            };
            let detail =
                service::request_json(mirage_ipc::Command::RepositoryDetail { repository_id })?;
            if detail["origin"].as_str() != Some("drive")
                || detail["volume_mode"].as_str() != Some("managed")
            {
                continue;
            }
            let Some(token) = capacity_drive_token(Some(client_credentials), token_store)? else {
                return Err(MirageError::invalid_argument(
                    "--client-credentials is required",
                ));
            };
            service::request_json(mirage_ipc::Command::DriveTokenSupply {
                repository_id,
                drive_access_token: token,
            })?;
            eprintln!("token-agent: supplied {repository_id}");
            supplied += 1;
        }
        if json {
            service::emit(
                serde_json::json!({"token_agent": true, "supplied": supplied}),
                true,
            )?;
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_seconds));
    }
}

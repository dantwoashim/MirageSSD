use clap::Subcommand;
use mirage_types::MirageError;

mod backend_login;
mod cache_check;
mod cache_verify;
mod config;
mod db_check;
mod device_backup;
mod device_drive;
mod drive_gate;
mod drive_live;
mod repo_drive;
mod repo_import_local;
mod repo_local;
mod repo_scan;
mod service;
mod simulate;
mod version;

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
    /// Mount a prepared repository.
    Mount {
        repository_id: mirage_types::RepositoryId,
        generation: mirage_types::GenerationId,
        /// Mount as an Explorer-visible read-only drive, for example M.
        #[arg(long)]
        drive_letter: Option<String>,
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
        client_credentials: std::path::PathBuf,
        #[arg(long)]
        token_store: Option<std::path::PathBuf>,
    },
    /// Run authenticated Drive Gates D and F with encrypted signed generations.
    GateDrive {
        #[arg(long)]
        client_credentials: std::path::PathBuf,
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
        client_credentials: std::path::PathBuf,
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
    },
    /// Repair recoverable local repository state.
    Repair {
        repository_id: mirage_types::RepositoryId,
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
            } => drive_live::verify(&client_credentials, token_store.as_deref(), json),
            BackendCommand::GateDrive {
                client_credentials,
                token_store,
                work_directory,
            } => drive_gate::run(
                &client_credentials,
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
                &client_credentials,
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
            } => repo_local::extract(
                &backend_root,
                &destination,
                repository_id,
                &key_id_hex,
                &test_key_hex,
                repository_key.as_deref(),
                json,
            ),
            RepoCommand::Repair { repository_id } => {
                service::run(mirage_ipc::Command::Repair { repository_id }, json)
            }
        },
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
        } => service::run(
            mirage_ipc::Command::Mount {
                repository_id,
                generation,
                drive_letter,
            },
            json,
        ),
        Command::Unmount { repository_id } => {
            service::run(mirage_ipc::Command::Unmount { repository_id }, json)
        }
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
    client_credentials
        .map(|credentials| {
            let session = drive_live::connect(credentials, token_store)?;
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

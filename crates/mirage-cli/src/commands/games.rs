//! Interactive-user controller. The privileged filesystem service must never
//! run the publisher's game or anti-cheat as LocalSystem/session zero.
use clap::Subcommand;
use mirage_ffi::sophon::{SophonSource, install, publisher, retention};
use mirage_types::MirageError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::process::{Command, Stdio};
#[cfg(windows)]
mod windows;

#[derive(Debug, Subcommand)]
pub enum GameCommand {
    /// Install official native files plus streamed asset presentation in a new managed root.
    Install {
        #[arg(long)]
        spec: PathBuf,
    },
    /// Inspect durable status and measured local allocation.
    Status {
        #[arg(long)]
        root: PathBuf,
    },
    /// Prepare learned chunks, launch publisher executable and clean up after actual game exit.
    Play {
        #[arg(long)]
        root: PathBuf,
    },
    /// Acquire and stage a replacement generation; retains previous native generation for rollback.
    Update {
        #[arg(long)]
        root: PathBuf,
    },
    /// Reacquire the current publisher generation into a fresh verified native tree.
    Repair {
        #[arg(long)]
        root: PathBuf,
    },
    /// Retry pending owned chunk cleanup once no game/source owner is active.
    Cleanup {
        #[arg(long)]
        root: PathBuf,
    },
    Rollback {
        #[arg(long)]
        root: PathBuf,
    },
    Policy {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        retention: String,
    },
    /// Remove only recorded native payloads of the previous generation; retain user files.
    Retire {
        #[arg(long)]
        root: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Balanced,
    Disposable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub root: PathBuf,
    pub seed_root: Option<PathBuf>,
    pub language: String,
    pub retention: Retention,
    pub total_budget_bytes: u64,
    pub cache_budget_bytes: u64,
    pub startup_budget_bytes: u64,
    pub disk_free_floor_bytes: u64,
    /// Optional existing official launcher. Without it launch the acquired
    /// publisher-native GenshinImpact.exe directly, retaining its normal login.
    pub launcher: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
struct State {
    format_version: u32,
    phase: String,
    active_generation: Option<PathBuf>,
    previous_generation: Option<PathBuf>,
    #[serde(default)]
    staged_generation: Option<PathBuf>,
    prepared: Vec<[u8; 32]>,
    error: Option<String>,
    host_pid: Option<u32>,
    #[serde(default)]
    host_creation_100ns: Option<u64>,
    game_pid: Option<u32>,
    reclaimed_bytes: u64,
}
fn source_error(status: mirage_ffi::MirageStatus) -> MirageError {
    MirageError::repository_conflict(format!("publisher game operation: {status:?}"))
}
fn read<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T, MirageError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(MirageError::from)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(MirageError::from)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(MirageError::invalid_argument("game state exceeds bound"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| MirageError::invalid_argument("invalid game state").with_source(e))
}
fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), MirageError> {
    ordinary(path)?;
    let temporary = path.with_extension("tmp");
    ordinary(&temporary)?;
    let mut file = File::create(&temporary).map_err(MirageError::from)?;
    file.write_all(
        &serde_json::to_vec_pretty(value).map_err(|e| {
            MirageError::internal_invariant("game state serialization").with_source(e)
        })?,
    )
    .and_then(|()| file.sync_all())
    .map_err(MirageError::from)?;
    drop(file);
    std::fs::rename(temporary, path).map_err(MirageError::from)
}
pub fn ordinary(path: &Path) -> Result<(), MirageError> {
    if !path.is_absolute()
        || path.to_string_lossy().contains(['*', '?'])
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(MirageError::invalid_argument(
            "game path must be absolute without traversal or wildcards",
        ));
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                let reparse = meta.file_type().is_symlink();
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    let reparse = reparse || meta.file_attributes() & 0x400 != 0;
                    if reparse {
                        return Err(MirageError::invalid_argument(
                            "owned game paths cannot traverse reparse points",
                        ));
                    }
                }
                if reparse {
                    return Err(MirageError::invalid_argument(
                        "owned game paths cannot traverse reparse points",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
impl Spec {
    fn validate(&self) -> Result<(), MirageError> {
        ordinary(&self.root)?;
        if self.root.parent().is_none()
            || self.root.file_name().is_none()
            || self.cache_budget_bytes < 4 * 1024 * 1024
            || self.cache_budget_bytes > self.total_budget_bytes
            || self.startup_budget_bytes > self.cache_budget_bytes
            || self.total_budget_bytes > 1024 * 1024 * 1024 * 1024
            || !["en-us", "ja-jp", "ko-kr", "zh-cn"].contains(&self.language.as_str())
        {
            return Err(MirageError::invalid_argument(
                "invalid game language, root or storage allowance",
            ));
        }
        if self.seed_root.as_ref().is_some_and(|seed| {
            !seed.is_absolute()
                || !seed.is_dir()
                || self.root.starts_with(seed)
                || seed.starts_with(&self.root)
        }) {
            return Err(MirageError::invalid_argument(
                "original installation must be separate from the managed root",
            ));
        }
        Ok(())
    }
}
fn lock(root: &Path) -> Result<File, MirageError> {
    ordinary(root)?;
    let path = root.join("worker.lock");
    ordinary(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(MirageError::from)?;
    file.try_lock().map_err(|_| {
        MirageError::repository_conflict("game operation or session is already running")
    })?;
    Ok(file)
}
fn save(root: &Path, state: &State) -> Result<(), MirageError> {
    write(&root.join("session.json"), state)
}
fn phase(root: &Path, state: &mut State, value: &str) -> Result<(), MirageError> {
    state.phase = value.into();
    state.error = None;
    save(root, state)
}

pub fn run(command: GameCommand, json_output: bool) -> Result<(), MirageError> {
    let result = match command {
        GameCommand::Install { spec } => {
            let spec: Spec = read(&spec)?;
            install_game(&spec)?;
            status(&spec.root)?
        }
        GameCommand::Status { root } => status(&root)?,
        GameCommand::Update { root } | GameCommand::Repair { root } => {
            maintenance(&root)?;
            status(&root)?
        }
        GameCommand::Cleanup { root } => {
            cleanup_game(&root)?;
            status(&root)?
        }
        GameCommand::Play { root } => {
            play(&root)?;
            status(&root)?
        }
        GameCommand::Rollback { root } => {
            rollback(&root)?;
            status(&root)?
        }
        GameCommand::Policy { root, retention } => {
            set_policy(&root, &retention)?;
            status(&root)?
        }
        GameCommand::Retire { root } => {
            retire_previous(&root)?;
            status(&root)?
        }
    };
    super::service::emit(result, json_output)
}

pub fn install_game(spec: &Spec) -> Result<(), MirageError> {
    spec.validate()?;
    if spec.root.exists() {
        return Err(MirageError::repository_conflict(
            "choose a new managed root; original installation remains read-only",
        ));
    }
    std::fs::create_dir(&spec.root).map_err(MirageError::from)?;
    let _owner = lock(&spec.root)?;
    write(&spec.root.join("game.json"), spec)?;
    let mut state = State {
        format_version: 1,
        phase: "installing".into(),
        active_generation: None,
        previous_generation: None,
        staged_generation: None,
        prepared: Vec::new(),
        error: None,
        host_pid: None,
        host_creation_100ns: None,
        game_pid: None,
        reclaimed_bytes: 0,
    };
    save(&spec.root, &state)?;
    let result = stage(spec, &mut state);
    if let Err(error) = &result {
        state.phase = "installation_failed".into();
        state.error = Some(error.message.clone());
        save(&spec.root, &state)?;
    }
    result
}

fn stage(spec: &Spec, state: &mut State) -> Result<(), MirageError> {
    discard_staged(&spec.root, state)?;
    if state.previous_generation.is_some() {
        return Err(MirageError::repository_conflict(
            "remove the previous native generation before staging another update; user files remain retained",
        ));
    }
    phase(&spec.root, state, "acquiring_metadata")?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| MirageError::internal_invariant("generation nonce"))?;
    let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let generation_root = spec.root.join("generations").join(name);
    ordinary(&generation_root)?;
    state.staged_generation = Some(generation_root.clone());
    save(&spec.root, state)?;
    std::fs::create_dir_all(&generation_root).map_err(MirageError::from)?;
    let generation = publisher::acquire(&generation_root.join("metadata"), &spec.language)
        .map_err(source_error)?;
    let inventory = install::inventory(&generation).map_err(source_error)?;
    let native_allocation = inventory.native_bytes.saturating_add(
        (inventory.native_files as u64).saturating_mul(
            retention::allocation_unit(&spec.root)
                .map_err(source_error)?
                .saturating_sub(1),
        ),
    );
    let occupied = physical_tree(&spec.root)?;
    let imported = match &spec.seed_root {
        Some(seed) => physical_tree(seed)?,
        None => 0,
    };
    let peak = occupied
        .saturating_add(imported)
        .checked_add(native_allocation)
        .and_then(|n| n.checked_add(spec.cache_budget_bytes))
        .and_then(|n| n.checked_add(4 * 1024 * 1024))
        .ok_or_else(|| MirageError::cache_full("game storage allowance overflow"))?;
    if peak > spec.total_budget_bytes {
        return Err(MirageError::cache_full(format!(
            "native files, retained generations and bounded cache need {peak} bytes; allowance is {}",
            spec.total_budget_bytes
        )));
    }
    let needed = native_allocation
        .saturating_add(spec.disk_free_floor_bytes)
        .saturating_add(4 * 1024 * 1024);
    if available(&spec.root)? < needed {
        return Err(MirageError::cache_full(
            "insufficient free space for staged native files plus free-space floor",
        ));
    }
    phase(&spec.root, state, "downloading_native_files")?;
    install::materialize_native(
        &generation,
        &generation_root.join("native"),
        spec.seed_root.iter().cloned().collect(),
        &generation_root.join("acquisition-cache"),
    )
    .map_err(source_error)?;
    // Preserve writable title data across native generations. These paths
    // remain ordinary local files and are never included in cache cleanup.
    let prior_native = state
        .active_generation
        .as_ref()
        .map(|p| p.join("native"))
        .or_else(|| spec.seed_root.clone());
    if let Some(prior) = prior_native {
        for name in ["GenshinImpact_Data/Persistent", "ScreenShot", "ScreenShots"] {
            let source = prior.join(name);
            if source.exists() {
                copy_user_tree(&source, &generation_root.join("native").join(name), spec)?;
            }
        }
    }
    let source_config = json!({ "manifests": generation.manifests, "export_prefixes": install::ASSET_PREFIXES,
        "seed_roots": spec.seed_root.iter().collect::<Vec<_>>(), "cache_root": spec.root.join("cache"),
        "disk_budget_bytes": spec.cache_budget_bytes, "disk_free_floor_bytes": spec.disk_free_floor_bytes, "memory_budget_bytes": 64 * 1024 * 1024 });
    write(&generation_root.join("source.json"), &source_config)?;
    write(&generation_root.join("inventory.json"), &inventory)?;
    // Validate the complete mounted generation before committing its pointer.
    let source = SophonSource::open(&generation_root.join("source.json")).map_err(source_error)?;
    drop(source);
    state.previous_generation = state.active_generation.take();
    state.active_generation = Some(generation_root);
    state.staged_generation = None;
    state.prepared.clear();
    phase(&spec.root, state, "ready")
}

fn discard_staged(root: &Path, state: &mut State) -> Result<(), MirageError> {
    let Some(staged) = state.staged_generation.clone() else {
        return Ok(());
    };
    ordinary(&staged)?;
    if staged.parent() != Some(root.join("generations").as_path())
        || state.active_generation.as_ref() == Some(&staged)
        || state.previous_generation.as_ref() == Some(&staged)
    {
        return Err(MirageError::invalid_argument(
            "unpublished generation escapes its recorded staging scope",
        ));
    }
    if staged.join("metadata/generation.json").is_file() {
        let generation: publisher::Generation = read(&staged.join("metadata/generation.json"))?;
        if generation.manifests.len() > 2
            || generation
                .manifests
                .iter()
                .any(|path| !path.starts_with(staged.join("metadata")))
        {
            return Err(MirageError::invalid_argument(
                "staging manifest escapes recorded generation",
            ));
        }
        for path in &generation.manifests {
            ordinary(path)?;
        }
        for name in install::native_paths(&generation).map_err(source_error)? {
            if ["GenshinImpact_Data/Persistent", "ScreenShot", "ScreenShots"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            let path = staged.join("native").join(name);
            ordinary(&path)?;
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    // These names are written only in the journaled unpublished scope. Leave
    // all other/user files and directories intact; no recursive tree deletion.
    for name in [
        "metadata/manifest_game.json",
        "metadata/manifest_en-us.json",
        "metadata/manifest_ja-jp.json",
        "metadata/manifest_ko-kr.json",
        "metadata/manifest_zh-cn.json",
        "metadata/generation.json",
        "source.json",
        "inventory.json",
        "native/config.ini",
        "acquisition-cache/native-acquisition.json",
    ] {
        let path = staged.join(name);
        ordinary(&path)?;
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    state.staged_generation = None;
    save(root, state)
}

fn copy_user_tree(source: &Path, target: &Path, spec: &Spec) -> Result<(), MirageError> {
    ordinary(source)?;
    ordinary(target)?;
    let required = physical_tree(source)?;
    let imported = match &spec.seed_root {
        Some(seed) => physical_tree(seed)?,
        None => 0,
    };
    let peak = physical_tree(&spec.root)?
        .saturating_add(imported)
        .saturating_add(required)
        .saturating_add(spec.cache_budget_bytes);
    if peak > spec.total_budget_bytes
        || available(&spec.root)? < required.saturating_add(spec.disk_free_floor_bytes)
    {
        return Err(MirageError::cache_full(
            "native update needs additional space to preserve user data",
        ));
    }
    let mut pending = vec![(source.to_path_buf(), target.to_path_buf())];
    let mut count = 0;
    while let Some((source, target)) = pending.pop() {
        ordinary(&source)?;
        ordinary(&target)?;
        std::fs::create_dir_all(&target).map_err(MirageError::from)?;
        for entry in std::fs::read_dir(&source).map_err(MirageError::from)? {
            count += 1;
            if count > 2_000_000 {
                return Err(MirageError::invalid_argument(
                    "user data copy exceeds file-count bound",
                ));
            }
            let entry = entry.map_err(MirageError::from)?;
            let destination = target.join(entry.file_name());
            ordinary(&entry.path())?;
            ordinary(&destination)?;
            if entry.file_type().map_err(MirageError::from)?.is_dir() {
                pending.push((entry.path(), destination));
            } else if entry.file_type().map_err(MirageError::from)?.is_file() {
                if destination.exists() {
                    return Err(MirageError::repository_conflict(
                        "publisher package overlaps preserved user data; active generation was not changed",
                    ));
                }
                std::fs::copy(entry.path(), destination).map_err(MirageError::from)?;
            } else {
                return Err(MirageError::invalid_argument("unsupported user data entry"));
            }
        }
    }
    Ok(())
}

pub fn maintenance(root: &Path) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let spec: Spec = read(&root.join("game.json"))?;
    spec.validate()?;
    let mut state: State = read(&root.join("session.json"))?;
    ensure_stopped(root, &state)?;
    let result = stage(&spec, &mut state);
    if let Err(error) = &result {
        state.phase = "maintenance_failed".into();
        state.error = Some(error.message.clone());
        save(root, &state)?;
    }
    result
}
pub fn set_policy(root: &Path, retention: &str) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let mut spec: Spec = read(&root.join("game.json"))?;
    spec.validate()?;
    if spec.root != root {
        return Err(MirageError::invalid_argument(
            "game registration root mismatch",
        ));
    }
    let state: State = read(&root.join("session.json"))?;
    ensure_stopped(root, &state)?;
    spec.retention = match retention {
        "balanced" => Retention::Balanced,
        "disposable" => Retention::Disposable,
        _ => return Err(MirageError::invalid_argument("unknown retention policy")),
    };
    write(&root.join("game.json"), &spec)
}
pub fn rollback(root: &Path) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let mut state: State = read(&root.join("session.json"))?;
    if ["retiring_previous", "retirement_failed"].contains(&state.phase.as_str()) {
        return Err(MirageError::repository_conflict(
            "previous-generation retirement must complete before rollback",
        ));
    }
    ensure_stopped(root, &state)?;
    let previous = state.previous_generation.clone().ok_or_else(|| {
        MirageError::repository_conflict("no previous native generation is retained")
    })?;
    let candidate = State {
        active_generation: Some(previous.clone()),
        ..read(&root.join("session.json"))?
    };
    generation(root, &candidate)?;
    let source = SophonSource::open(&previous.join("source.json")).map_err(source_error)?;
    drop(source);
    state.previous_generation = state.active_generation.take();
    state.active_generation = Some(previous);
    state.prepared.clear();
    phase(root, &mut state, "ready")
}
pub fn retire_previous(root: &Path) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let mut state: State = read(&root.join("session.json"))?;
    ensure_stopped(root, &state)?;
    let previous = state
        .previous_generation
        .clone()
        .ok_or_else(|| MirageError::repository_conflict("no previous generation to retire"))?;
    let candidate = State {
        active_generation: Some(previous.clone()),
        ..read(&root.join("session.json"))?
    };
    generation(root, &candidate)?;
    if state.active_generation.as_ref() == Some(&previous) {
        return Err(MirageError::invalid_argument(
            "cannot retire the active generation",
        ));
    }
    let generation: publisher::Generation = read(&previous.join("metadata/generation.json"))?;
    if generation.manifests.len() > 2
        || generation
            .manifests
            .iter()
            .any(|path| !path.starts_with(previous.join("metadata")))
    {
        return Err(MirageError::invalid_argument(
            "retirement manifest escapes generation",
        ));
    }
    for path in &generation.manifests {
        ordinary(path)?;
    }
    phase(root, &mut state, "retiring_previous")?;
    let before = physical_tree(root)?;
    let result = (|| -> Result<(), MirageError> {
        for name in install::native_paths(&generation).map_err(source_error)? {
            if ["GenshinImpact_Data/Persistent", "ScreenShot", "ScreenShots"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            let path = previous.join("native").join(name);
            ordinary(&path)?;
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    })();
    state.reclaimed_bytes = before.saturating_sub(physical_tree(root)?);
    if let Err(error) = &result {
        state.phase = "retirement_failed".into();
        state.error = Some(error.message.clone());
        save(root, &state)?;
    } else {
        state.previous_generation = None;
        phase(root, &mut state, "ready")?;
    }
    result
}
fn generation(root: &Path, state: &State) -> Result<PathBuf, MirageError> {
    let active = state
        .active_generation
        .as_ref()
        .ok_or_else(|| MirageError::repository_conflict("installation is incomplete"))?;
    ordinary(active)?;
    if active.parent() != Some(root.join("generations").as_path()) {
        return Err(MirageError::invalid_argument(
            "generation escapes managed root",
        ));
    }
    Ok(active.clone())
}
fn ensure_stopped(root: &Path, state: &State) -> Result<(), MirageError> {
    #[cfg(windows)]
    {
        if windows::game_running(root)? {
            return Err(MirageError::repository_conflict(
                "a game/source process remains active; close it before maintenance or recovery",
            ));
        }
        windows::recover_orphan(root, state)?;
    }
    #[cfg(not(windows))]
    {
        let _ = (root, state);
    }
    Ok(())
}
pub fn cleanup_game(root: &Path) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let spec: Spec = read(&root.join("game.json"))?;
    spec.validate()?;
    let mut state: State = read(&root.join("session.json"))?;
    ensure_stopped(root, &state)?;
    discard_staged(root, &mut state)?;
    do_cleanup(&spec, &mut state)
}
fn do_cleanup(spec: &Spec, state: &mut State) -> Result<(), MirageError> {
    phase(&spec.root, state, "cleanup_pending")?;
    let keep = match spec.retention {
        Retention::Balanced => state.prepared.clone(),
        Retention::Disposable => Vec::new(),
    };
    let report = if spec.root.join("cache/owner.lock").is_file() {
        retention::cleanup(&spec.root.join("cache"), keep).map_err(source_error)?
    } else {
        retention::Cleanup::default()
    };
    state.reclaimed_bytes = report.reclaimed_bytes;
    state.host_pid = None;
    state.game_pid = None;
    if report.pending {
        state.error = Some(
            "some owned chunks remain locked; cleanup will retry before the next mount".into(),
        );
        save(&spec.root, state)
    } else {
        phase(&spec.root, state, "ready")
    }
}

pub fn status(root: &Path) -> Result<Value, MirageError> {
    ordinary(root)?;
    if !root.join("game.json").is_file() {
        return Ok(json!({"registered": false}));
    }
    let spec: Spec = read(&root.join("game.json"))?;
    spec.validate()?;
    let state: State = read(&root.join("session.json"))?;
    let total = physical_tree(root)?;
    let cache = if root.join("cache").exists() {
        physical_tree(&root.join("cache"))?
    } else {
        0
    };
    let imported = match &spec.seed_root {
        Some(seed) => physical_tree(seed)?,
        None => 0,
    };
    let active = state.active_generation.as_ref();
    let native = match active {
        Some(active) if active.join("native").exists() => physical_tree(&active.join("native"))?,
        _ => 0,
    };
    let inventory: Option<install::Inventory> =
        active.and_then(|active| read(&active.join("inventory.json")).ok());
    let busy = lock(root).is_err();
    Ok(
        json!({ "registered": true, "phase": state.phase, "busy": busy, "error": state.error, "rollback_available": state.previous_generation.is_some() && !["retiring_previous", "retirement_failed"].contains(&state.phase.as_str()), "retirement_available": state.previous_generation.is_some(),
        "root": root, "native_root": active.map(|p| p.join("native")), "language": spec.language, "retention": spec.retention,
        "allowance_bytes": spec.total_budget_bytes, "cache_budget_bytes": spec.cache_budget_bytes,
        "native_bytes": native, "owned_cache_and_metadata_bytes": cache, "temporary_and_rollback_bytes": total.saturating_sub(native).saturating_sub(cache),
        "imported_original_bytes": imported, "physical_total_bytes": total.saturating_add(imported), "owned_physical_bytes": total,
        "virtual_logical_bytes": inventory.map(|i| i.virtual_bytes), "available_bytes": available(root)?, "last_reclaimed_bytes": state.reclaimed_bytes,
        "gameplay_qualified": false }),
    )
}

#[derive(Serialize, Deserialize)]
struct Job {
    root: PathBuf,
    pid: u32,
    creation: Option<u64>,
    operation: String,
    log: PathBuf,
}
fn bridge_root() -> Result<PathBuf, MirageError> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        MirageError::provider_unavailable("interactive local app data unavailable")
    })?;
    let path = PathBuf::from(base).join("MirageSSD").join("games");
    ordinary(&path)?;
    std::fs::create_dir_all(&path).map_err(MirageError::from)?;
    Ok(path)
}

/// Token-authenticated desktop bridge uses a detached interactive-user helper.
/// Ownership is enforced again by the worker lock, so UI closure or a second
/// app window cannot race a playing session with maintenance/cleanup.
#[cfg(windows)]
pub fn start_job(operation: &str, payload: Value) -> Result<Value, MirageError> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
    let bridge = bridge_root()?;
    let _bridge_owner = lock(&bridge)?;
    if let Ok(job) = read::<Job>(&bridge.join("job.json"))
        && windows::pid_alive(job.pid, job.creation)
    {
        return Err(MirageError::repository_conflict(
            "a game task is already running",
        ));
    }
    let mut arguments = vec!["game".to_owned(), operation.to_owned()];
    let root = if operation == "install" {
        let spec: Spec = serde_json::from_value(payload).map_err(|error| {
            MirageError::invalid_argument("invalid game setup").with_source(error)
        })?;
        spec.validate()?;
        if spec.root.exists() {
            return Err(MirageError::repository_conflict(
                "choose a new managed folder",
            ));
        }
        let input = bridge.join("install-input.json");
        write(&input, &spec)?;
        arguments.extend(["--spec".into(), input.to_string_lossy().into_owned()]);
        spec.root
    } else {
        if ![
            "play", "update", "repair", "cleanup", "rollback", "policy", "retire",
        ]
        .contains(&operation)
        {
            return Err(MirageError::invalid_argument("unknown game operation"));
        }
        let job: Job = read(&bridge.join("job.json"))?;
        ordinary(&job.root)?;
        arguments.extend(["--root".into(), job.root.to_string_lossy().into_owned()]);
        if operation == "policy" {
            let retention = payload["retention"]
                .as_str()
                .filter(|value| ["balanced", "disposable"].contains(value))
                .ok_or_else(|| MirageError::invalid_argument("unknown retention policy"))?;
            arguments.extend(["--retention".into(), retention.into()]);
        }
        job.root
    };
    let executable = std::env::current_exe().map_err(MirageError::from)?;
    let worker = executable
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("UI executable location"))?
        .join("mirage.exe");
    let log = bridge.join("last-operation.log");
    ordinary(&log)?;
    let output = File::create(&log).map_err(MirageError::from)?;
    let child = Command::new(&worker)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone().map_err(MirageError::from)?))
        .stderr(Stdio::from(output))
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .map_err(MirageError::from)?;
    let job = Job {
        root,
        pid: child.id(),
        creation: windows::creation(child.id()),
        operation: operation.into(),
        log,
    };
    write(&bridge.join("job.json"), &job)?;
    Ok(json!({"started": true, "root": job.root}))
}
#[cfg(not(windows))]
pub fn start_job(_operation: &str, _payload: Value) -> Result<Value, MirageError> {
    Err(MirageError::provider_unavailable(
        "game integration requires Windows",
    ))
}

pub fn bridge_status() -> Result<Value, MirageError> {
    let bridge = bridge_root()?;
    let job = match read::<Job>(&bridge.join("job.json")) {
        Ok(job) => job,
        Err(_) => return Ok(json!({"registered": false, "busy": false})),
    };
    let mut value = status(&job.root)?;
    #[cfg(windows)]
    {
        value["busy"] = json!(
            value["busy"].as_bool() == Some(true) || windows::pid_alive(job.pid, job.creation)
        );
    }
    value["operation"] = json!(job.operation);
    if value["busy"] != true && value["registered"] != true {
        let mut text = String::new();
        File::open(&job.log)
            .map_err(MirageError::from)?
            .take(8192)
            .read_to_string(&mut text)
            .map_err(MirageError::from)?;
        value["error"] = json!(text);
    }
    Ok(value)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn spec(root: &Path) -> Spec {
        Spec {
            root: root.into(),
            seed_root: None,
            language: "en-us".into(),
            retention: Retention::Balanced,
            total_budget_bytes: 64 * 1024 * 1024,
            cache_budget_bytes: 4 * 1024 * 1024,
            startup_budget_bytes: 0,
            disk_free_floor_bytes: 0,
            launcher: None,
        }
    }
    fn empty_state(active: PathBuf, previous: PathBuf) -> State {
        State {
            format_version: 1,
            phase: "ready".into(),
            active_generation: Some(active),
            previous_generation: Some(previous),
            staged_generation: None,
            prepared: Vec::new(),
            error: None,
            host_pid: None,
            host_creation_100ns: None,
            game_pid: None,
            reclaimed_bytes: 0,
        }
    }
    #[test]
    fn locks_paths_and_registration_budgets_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let first = lock(root.path()).unwrap();
        assert!(lock(root.path()).is_err());
        drop(first);
        assert!(lock(root.path()).is_ok());
        assert!(ordinary(&root.path().join("../escape")).is_err());
        assert!(ordinary(Path::new("relative")).is_err());
        let mut spec = spec(root.path());
        spec.startup_budget_bytes = spec.cache_budget_bytes + 1;
        assert!(spec.validate().is_err());
        spec.startup_budget_bytes = 0;
        spec.language = "unsupported".into();
        assert!(spec.validate().is_err());
    }
    #[test]
    fn user_data_copy_preserves_originals_and_rejects_package_overlap() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let managed = root.path().join("managed");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&managed).unwrap();
        std::fs::write(original.join("settings.dat"), b"keep user settings").unwrap();
        let target = managed.join("generation/Persistent");
        copy_user_tree(&original, &target, &spec(&managed)).unwrap();
        assert_eq!(
            std::fs::read(target.join("settings.dat")).unwrap(),
            b"keep user settings"
        );
        std::fs::write(target.join("settings.dat"), b"verified publisher bytes").unwrap();
        assert!(copy_user_tree(&original, &target, &spec(&managed)).is_err());
        assert_eq!(
            std::fs::read(target.join("settings.dat")).unwrap(),
            b"verified publisher bytes"
        );
        assert_eq!(
            std::fs::read(original.join("settings.dat")).unwrap(),
            b"keep user settings"
        );
    }
    #[test]
    fn retirement_removes_only_previous_recorded_native_payloads() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let current = root.join("generations/current");
        let previous = root.join("generations/previous");
        std::fs::create_dir_all(current.join("native")).unwrap();
        std::fs::create_dir_all(previous.join("native/GenshinImpact_Data/Persistent")).unwrap();
        std::fs::create_dir(previous.join("metadata")).unwrap();
        std::fs::write(
            current.join("native/GenshinImpact.exe"),
            b"current protected",
        )
        .unwrap();
        std::fs::write(previous.join("native/GenshinImpact.exe"), vec![1; 8192]).unwrap();
        std::fs::write(
            previous.join("native/GenshinImpact_Data/Persistent/settings"),
            b"user",
        )
        .unwrap();
        std::fs::write(previous.join("native/unknown-user-file"), b"user").unwrap();
        let manifest = previous.join("metadata/manifest.json");
        write(&manifest, &json!({"chunk_url_prefix":"https://fixture.invalid/chunks","assets":[{"name":"GenshinImpact.exe","size":8192,"md5":"00000000000000000000000000000000","chunks":[]},{"name":"GenshinImpact_Data/Persistent/settings","size":4,"md5":"00000000000000000000000000000000","chunks":[]}]})).unwrap();
        write(
            &previous.join("metadata/generation.json"),
            &publisher::Generation {
                version: "1.0.0".into(),
                build_id: "fixture".into(),
                language: "en-us".into(),
                manifests: vec![manifest],
                logical_bytes: 8196,
            },
        )
        .unwrap();
        write(&root.join("game.json"), &spec(root)).unwrap();
        save(root, &empty_state(current.clone(), previous.clone())).unwrap();
        retire_previous(root).unwrap();
        assert!(!previous.join("native/GenshinImpact.exe").exists());
        assert!(current.join("native/GenshinImpact.exe").exists());
        assert!(
            previous
                .join("native/GenshinImpact_Data/Persistent/settings")
                .exists()
        );
        assert!(previous.join("native/unknown-user-file").exists());
        let state: State = read(&root.join("session.json")).unwrap();
        assert!(state.previous_generation.is_none());
        assert_eq!(state.active_generation, Some(current));
    }
    #[test]
    fn failed_maintenance_and_invalid_rollback_keep_active_generation_and_user_files() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("generations/current");
        let previous = root.path().join("generations/previous");
        std::fs::create_dir_all(current.join("native/GenshinImpact_Data/Persistent")).unwrap();
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(
            current.join("native/GenshinImpact_Data/Persistent/settings"),
            b"keep",
        )
        .unwrap();
        write(&root.path().join("game.json"), &spec(root.path())).unwrap();
        save(root.path(), &empty_state(current.clone(), previous)).unwrap();
        // A retained previous generation blocks new staging before any network
        // request, preventing untracked rollback accumulation.
        assert!(maintenance(root.path()).is_err());
        assert!(rollback(root.path()).is_err());
        let state: State = read(&root.path().join("session.json")).unwrap();
        assert_eq!(state.active_generation, Some(current.clone()));
        assert_eq!(
            std::fs::read(current.join("native/GenshinImpact_Data/Persistent/settings")).unwrap(),
            b"keep"
        );
    }
    #[test]
    fn repeated_partial_stage_recovery_reclaims_only_unpublished_owned_payloads() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let active = root.join("generations/active");
        std::fs::create_dir_all(active.join("native")).unwrap();
        std::fs::write(active.join("native/GenshinImpact.exe"), b"protected active").unwrap();
        let mut state = empty_state(active.clone(), root.join("generations/rollback"));
        let initial = physical_tree(root).unwrap();
        for index in 0..3 {
            let staged = root.join(format!("generations/staged{index}"));
            std::fs::create_dir_all(staged.join("native/GenshinImpact_Data/Persistent")).unwrap();
            std::fs::create_dir(staged.join("metadata")).unwrap();
            std::fs::write(
                staged.join("native/GenshinImpact.exe"),
                vec![1; 1024 * 1024],
            )
            .unwrap();
            std::fs::write(staged.join("native/unknown-user-file"), b"keep").unwrap();
            std::fs::write(
                staged.join("native/GenshinImpact_Data/Persistent/settings"),
                b"keep",
            )
            .unwrap();
            let manifest = staged.join("metadata/manifest_game.json");
            write(&manifest, &json!({"chunk_url_prefix":"https://fixture.invalid/chunks","assets":[{"name":"GenshinImpact.exe","size":1048576,"md5":"00000000000000000000000000000000","chunks":[]}]})).unwrap();
            write(
                &staged.join("metadata/generation.json"),
                &publisher::Generation {
                    version: "1.0.0".into(),
                    build_id: "fixture".into(),
                    language: "en-us".into(),
                    manifests: vec![manifest],
                    logical_bytes: 1048576,
                },
            )
            .unwrap();
            state.staged_generation = Some(staged.clone());
            save(root, &state).unwrap();
            discard_staged(root, &mut state).unwrap();
            assert!(!staged.join("native/GenshinImpact.exe").exists());
            assert!(staged.join("native/unknown-user-file").exists());
            assert!(
                staged
                    .join("native/GenshinImpact_Data/Persistent/settings")
                    .exists()
            );
            assert_eq!(state.active_generation.as_ref(), Some(&active));
            assert!(state.previous_generation.is_some());
            assert!(
                physical_tree(root).unwrap() < initial + 65536,
                "repeated partial payloads must not accumulate"
            );
        }
    }
}

#[cfg(windows)]
pub fn physical_tree(root: &Path) -> Result<u64, MirageError> {
    windows::physical_tree(root)
}
#[cfg(not(windows))]
pub fn physical_tree(_root: &Path) -> Result<u64, MirageError> {
    Err(MirageError::provider_unavailable(
        "physical game allocation requires Windows",
    ))
}
#[cfg(windows)]
fn available(root: &Path) -> Result<u64, MirageError> {
    windows::available(root)
}
#[cfg(not(windows))]
fn available(_root: &Path) -> Result<u64, MirageError> {
    Err(MirageError::provider_unavailable(
        "game installation requires Windows",
    ))
}
#[cfg(windows)]
pub fn play(root: &Path) -> Result<(), MirageError> {
    windows::play(root)
}
#[cfg(not(windows))]
pub fn play(_root: &Path) -> Result<(), MirageError> {
    Err(MirageError::provider_unavailable(
        "game sessions require Windows",
    ))
}

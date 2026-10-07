//! Repository registration and lifecycle configuration commands.

use super::config::RUNTIME_FORMAT_VERSION;
use super::config::save_config;
use super::config::validate_runtime_fields;
use super::conversion::verify_local_import;
use super::drive::active_generation;
use super::drive::local_commit_hash;
use super::fs_util::canonical_directory;
use super::fs_util::validate_label;
use super::fs_util::validate_launcher;
use super::fs_util::write_atomic;
use super::materialize::DEFAULT_DRAIN_MS;
use super::mount::validate_mount_subtree;
use super::*;

pub fn register(
    database: &Database,
    owner_sid: &str,
    spec: RegisterSpec,
) -> Result<Value, MirageError> {
    validate_label(&spec.display_name, "repository display name")?;
    let native_root = canonical_directory(&spec.native_root, "native game root")?;
    let (mount_subtree, mount_root) = validate_mount_subtree(&native_root, &spec.mount_subtree)?;
    let import_root = canonical_directory(&spec.import_root, "import root")?;
    let launcher_relative = validate_launcher(&native_root, &spec.launcher_relative)?;
    validate_runtime_fields(
        &spec.arguments,
        &spec.version_label,
        &spec.configuration_label,
        spec.cache_bytes,
    )?;

    let manifest_path = import_root.join("base-manifest.cbor");
    let manifest_bytes = bounded_read(&manifest_path, DecodeLimits::default().max_input_bytes)?;
    let manifest = decode_manifest_bounded(&manifest_bytes, DecodeLimits::default())?;
    if manifest.repository_id != spec.repository_id {
        return Err(MirageError::repository_conflict(
            "import manifest repository ID differs from registration",
        ));
    }
    let content_encrypted = verify_local_import(&manifest, &import_root)?;
    let manifest_hash = manifest_hash(&manifest)?;
    let index_bytes = compile_to_bytes(&manifest)?;
    let commit_hash = local_commit_hash(&manifest, &import_root)?;

    let existing_owner = database.load_repository_owner_sid(spec.repository_id)?;
    if existing_owner
        .as_deref()
        .is_some_and(|owner| owner != owner_sid)
    {
        return Err(MirageError::backend_permission_denied(
            "repository is already owned by another Windows SID",
        ));
    }
    if let Some(existing_encrypted) =
        database.load_repository_content_encrypted(spec.repository_id)?
        && existing_encrypted != content_encrypted
    {
        return Err(MirageError::repository_conflict(
            "repository encryption policy cannot be changed by registration",
        ));
    }

    let repository_root = repository_state_root(database, spec.repository_id)?;
    std::fs::create_dir_all(&repository_root).map_err(MirageError::from)?;
    let index_path = repository_root.join("mount-index.bin");
    write_atomic(&index_path, &index_bytes)?;

    match existing_owner {
        Some(_) => {
            let existing_root = database
                .load_repository_root(spec.repository_id)?
                .ok_or_else(|| MirageError::integrity_mismatch("repository root disappeared"))?;
            if existing_root != mount_root {
                return Err(MirageError::repository_conflict(
                    "registered mount subtree differs from the existing repository",
                ));
            }
        }
        None => database.create_repository(NewRepository {
            repository_id: spec.repository_id,
            display_name: spec.display_name,
            local_root: mount_root,
            owner_sid: owner_sid.to_owned(),
            content_encrypted,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: now_ns(),
        })?,
    }

    let generation = VerifiedGeneration {
        repository_id: spec.repository_id,
        generation_id: manifest.generation_id,
        commit_hash,
        manifest_hash,
        manifest_local_path: manifest_path,
        mount_index_path: Some(index_path),
        created_at_ns: now_ns(),
    };
    database.insert_verified_generation(generation.clone())?;
    match database.load_active_generation(spec.repository_id)? {
        Some(active)
            if active.generation_id == generation.generation_id
                && active.commit_hash == generation.commit_hash => {}
        Some(_) => {
            return Err(MirageError::repository_conflict(
                "repository already has a different active generation",
            ));
        }
        None => {
            database.activate_generation(
                spec.repository_id,
                generation.generation_id,
                generation.commit_hash,
                None,
                now_ns(),
            )?;
            // Seed the durable namespace so inode lookups survive restarts;
            // idempotent when the volume was already seeded.
            let nodes = mirage_manifest::builder::namespace_seed(&manifest)
                .into_iter()
                .map(|entry| mirage_db::NamespaceSeedNode {
                    path: entry.path,
                    is_directory: entry.is_directory,
                    size: entry.size,
                    version_root: None,
                })
                .collect();
            database
                .writer()
                .namespace_seed(spec.repository_id, nodes, now_ns())?;
        }
    }

    let config = RuntimeConfig {
        format_version: RUNTIME_FORMAT_VERSION,
        origin: RuntimeOrigin::Local,
        native_root,
        mount_subtree,
        import_root,
        launcher_relative,
        arguments: spec.arguments,
        version_label: spec.version_label,
        configuration_label: spec.configuration_label,
        cache_bytes: spec.cache_bytes,
        drain_ms: DEFAULT_DRAIN_MS,
        conversion: None,
    };
    save_config(database, spec.repository_id, &config)?;
    Ok(json!({
        "repository_id": spec.repository_id.to_string(),
        "owner_sid_bound": true,
        "generation": manifest.generation_id.as_u64(),
        "manifest_hash": manifest_hash.to_string(),
        "page_count": manifest.pages.len(),
        "cache_bytes": config.cache_bytes,
        "mount_subtree": config.mount_subtree,
        "content_encrypted": content_encrypted,
        "state": RepositoryState::ReadyUnmounted.as_str()
    }))
}

pub fn configure(
    database: &Database,
    repository_id: RepositoryId,
    launcher_relative: PathBuf,
    arguments: Vec<String>,
    version_label: String,
    configuration_label: String,
) -> Result<Value, MirageError> {
    let mut config = load_config(database, repository_id)?;
    config.launcher_relative = validate_launcher(&config.native_root, &launcher_relative)?;
    validate_runtime_fields(
        &arguments,
        &version_label,
        &configuration_label,
        config.cache_bytes,
    )?;
    config.arguments = arguments;
    config.version_label = version_label;
    config.configuration_label = configuration_label;
    save_config(database, repository_id, &config)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "configured": true,
        "launcher_relative": config.launcher_relative,
        "version_label": config.version_label,
        "configuration_label": config.configuration_label
    }))
}

pub fn set_drive_origin(
    database: &Database,
    repository_id: RepositoryId,
    drive: bool,
) -> Result<Value, MirageError> {
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyUnmounted {
        return Err(MirageError::repository_conflict(
            "repository origin can change only while ready and unmounted",
        ));
    }
    let active = active_generation(database, repository_id)?;
    let local_manifest = decode_manifest_bounded(
        &bounded_read(
            &active.manifest_local_path,
            DecodeLimits::default().max_input_bytes,
        )?,
        DecodeLimits::default(),
    )?;
    let mut config = load_config(database, repository_id)?;
    config.origin = if drive {
        if database.load_repository_content_encrypted(repository_id)? != Some(true) {
            return Err(MirageError::backend_permission_denied(
                "Drive origin requires an encrypted repository",
            ));
        }
        let _ = load_drive_manifest(&config, &local_manifest)?;
        RuntimeOrigin::Drive
    } else {
        verify_local_import(&local_manifest, &config.import_root)?;
        RuntimeOrigin::Local
    };
    save_config(database, repository_id, &config)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "origin": config.origin.as_str(),
        "cloud_reads_in_filesystem_callbacks": false
    }))
}

/// Persists the repository's volume mode. Switching is refused while the
/// repository is mounted so a live host never changes contract underneath
/// the kernel client.
pub fn set_volume_mode(
    database: &Database,
    repository_id: RepositoryId,
    managed: bool,
) -> Result<Value, MirageError> {
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyUnmounted {
        return Err(MirageError::repository_conflict(
            "repository volume mode can change only while ready and unmounted",
        ));
    }
    let mode = if managed {
        mirage_db::VolumeMode::Managed
    } else {
        mirage_db::VolumeMode::Legacy
    };
    database.set_repository_volume_mode(repository_id, mode)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "volume_mode": mode.as_str()
    }))
}

//! Service job launchers for drive login, offload, cache, and create.

use super::*;

pub(super) fn drive_status(
    shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
) -> serde_json::Value {
    let login = shared
        .lock()
        .map(|state| match &state.login {
            LoginState::Idle => serde_json::json!("idle"),
            LoginState::InFlight => serde_json::json!("in_flight"),
            LoginState::Done { account_id } => {
                serde_json::json!({"done": account_id})
            }
            LoginState::Failed { message } => serde_json::json!({"failed": message}),
        })
        .unwrap_or(serde_json::json!("idle"));
    let store = mirage_cli::commands::backend_login::default_token_store_path()
        .map(mirage_backend_drive::token_store::TokenStore::new);
    let metadata = store.ok().and_then(|store| store.metadata().ok());
    serde_json::json!({
        "authenticated": metadata.is_some(),
        "account_id": metadata.as_ref().map(|m| m.account_id.clone()),
        "issued_unix_seconds": metadata.as_ref().map(|m| m.issued_unix_seconds),
        "login": login,
    })
}

pub(super) fn start_drive_login(
    shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
) -> serde_json::Value {
    {
        let mut state = match shared.lock() {
            Ok(state) => state,
            Err(_) => return serde_json::json!({"error": "state lock poisoned"}),
        };
        if matches!(state.login, LoginState::InFlight) {
            return serde_json::json!({"started": false, "in_flight": true});
        }
        state.login = LoginState::InFlight;
        state.login_cancel = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
    }
    let cancel = shared.lock().ok().and_then(|s| s.login_cancel.clone());
    std::thread::spawn({
        // The handler thread is parked on a socket; spawn a worker for
        // the blocking OAuth exchange.
        let shared = shared.clone();
        move || {
            let flag = cancel.clone();
            let result = mirage_cli::commands::backend_login::authenticate_cancellable(
                None, None, None, 600, cancel,
            );
            if let Ok(mut state) = shared.lock() {
                let cancelled =
                    flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
                state.login_cancel = None;
                state.login = match result {
                    _ if cancelled => LoginState::Idle,
                    Ok(outcome) => LoginState::Done {
                        account_id: outcome.account_id,
                    },
                    Err(error) => {
                        let message = if error.to_string().contains("access_denied")
                            || error.to_string().contains("permission_denied")
                        {
                            "Google declined access for this account. Try again or use a different account."
                                .to_owned()
                        } else {
                            error.to_string()
                        };
                        LoginState::Failed { message }
                    }
                };
            }
        }
    });
    serde_json::json!({"started": true})
}

/// Offloads a local folder into a drive on a worker thread; progress and
/// the final report are polled through offload.
pub(super) fn start_volume_offload(
    shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, MirageError> {
    let repository_id = payload["repository_id"]
        .as_str()
        .ok_or_else(|| MirageError::invalid_argument("repository_id is required"))?
        .parse::<mirage_types::RepositoryId>()?;
    let source = payload["source"]
        .as_str()
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| MirageError::invalid_argument("source folder is required"))?;
    let delete_source = payload["delete_source"].as_bool().unwrap_or(false);
    {
        let mut state = shared
            .lock()
            .map_err(|_| MirageError::internal_invariant("state lock poisoned"))?;
        if state.offload["in_flight"].as_bool() == Some(true) {
            return Ok(serde_json::json!({"started": false, "in_flight": true}));
        }
        state.offload = serde_json::json!({
            "in_flight": true,
            "step": "starting",
            "repository_id": repository_id.to_string(),
            "source": source.to_string_lossy(),
        });
    }
    std::thread::spawn({
        let shared = shared.clone();
        move || {
            let progress_shared = shared.clone();
            let mut progress = move |step: &str| {
                if let Ok(mut state) = progress_shared.lock() {
                    state.offload["step"] = serde_json::json!(step);
                }
            };
            let result = mirage_cli::commands::offload::run(
                &mirage_cli::commands::offload::OffloadSpec {
                    repository_id,
                    source: source.clone(),
                    destination: None,
                    delete_source,
                    // The UI keeps polling; a day bounds a very large folder.
                    wait_publish: std::time::Duration::from_secs(86_400),
                },
                None,
                &mut progress,
            );
            if let Ok(mut state) = shared.lock() {
                state.offload = match result.and_then(|report| {
                    serde_json::to_value(report).map_err(|error| {
                        MirageError::internal_invariant(format!("offload report: {error}"))
                    })
                }) {
                    Ok(mut value) => {
                        value["in_flight"] = serde_json::json!(false);
                        value["done"] = serde_json::json!(true);
                        value["repository_id"] = serde_json::json!(repository_id.to_string());
                        value["source"] = serde_json::json!(source.to_string_lossy());
                        value
                    }
                    Err(error) => serde_json::json!({
                        "in_flight": false,
                        "done": false,
                        "repository_id": repository_id.to_string(),
                        "source": source.to_string_lossy(),
                        "error": error.to_string(),
                    }),
                };
            }
        }
    });
    Ok(serde_json::json!({"started": true}))
}

/// Moves a volume's local cache to another disk on a worker thread;
/// progress and the outcome are polled through set_cache.
pub(super) fn start_volume_set_cache(
    shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    payload: &serde_json::Value,
    drive_token_store: Option<&Path>,
) -> Result<serde_json::Value, MirageError> {
    let repository_id = payload["repository_id"]
        .as_str()
        .ok_or_else(|| MirageError::invalid_argument("repository_id is required"))?
        .parse::<mirage_types::RepositoryId>()?;
    let cache_disk = payload["cache_disk"]
        .as_str()
        .map(str::trim)
        .filter(|disk| !disk.is_empty())
        .map(str::to_owned);
    {
        let mut state = shared
            .lock()
            .map_err(|_| MirageError::internal_invariant("state lock poisoned"))?;
        if state.set_cache["in_flight"].as_bool() == Some(true) {
            return Ok(serde_json::json!({"started": false, "in_flight": true}));
        }
        state.set_cache = serde_json::json!({
            "in_flight": true,
            "step": "starting",
            "repository_id": repository_id.to_string(),
        });
    }
    let token_store = drive_token_store.map(Path::to_path_buf);
    std::thread::spawn({
        let shared = shared.clone();
        move || {
            let progress_shared = shared.clone();
            let mut progress = move |step: &str| {
                if let Ok(mut state) = progress_shared.lock() {
                    state.set_cache["step"] = serde_json::json!(step);
                }
            };
            // A remount needs a Drive token; without one the move still
            // completes and the agent remounts when it next signs in.
            let token = futures_executor::block_on(mirage_backend_drive::refresh_stored_session(
                token_store.as_deref(),
            ))
            .ok()
            .and_then(|session| {
                mirage_ipc::SensitiveString::new(session.access_token.as_str().to_owned()).ok()
            });
            let result = mirage_cli::commands::volume::set_cache(
                repository_id,
                cache_disk.as_deref(),
                token,
                None,
                &mut progress,
            );
            if let Ok(mut state) = shared.lock() {
                state.set_cache = match result {
                    Ok(mut value) => {
                        value["in_flight"] = serde_json::json!(false);
                        value["done"] = serde_json::json!(true);
                        value
                    }
                    Err(error) => serde_json::json!({
                        "in_flight": false,
                        "done": false,
                        "repository_id": repository_id.to_string(),
                        "error": error.to_string(),
                    }),
                };
            }
        }
    });
    Ok(serde_json::json!({"started": true}))
}

pub(super) fn start_volume_create(
    shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, MirageError> {
    {
        let mut state = shared
            .lock()
            .map_err(|_| MirageError::internal_invariant("state lock poisoned"))?;
        if state.create["in_flight"].as_bool() == Some(true) {
            return Ok(serde_json::json!({"started": false, "in_flight": true}));
        }
        state.create = serde_json::json!({"in_flight": true, "step": "starting"});
    }
    let spec = mirage_cli::commands::volume::VolumeSpec {
        name: payload["name"]
            .as_str()
            .unwrap_or(mirage_cli::commands::volume::default_volume_name())
            .to_owned(),
        drive_letter: payload["letter"].as_str().unwrap_or("M").to_owned(),
        budget_bytes: payload["budget_bytes"]
            .as_u64()
            .map(Ok)
            .unwrap_or_else(mirage_cli::commands::volume::default_budget_bytes)
            .unwrap_or(8 * (1 << 30)),
        cache_disk: match payload["cache_disk"].as_str() {
            Some(disk) if !disk.trim().is_empty() => Some(disk.trim().to_owned()),
            _ => mirage_cli::commands::volume::default_cache_disk().unwrap_or(None),
        },
        floor_bytes: payload["floor_bytes"].as_u64(),
    };
    // The floor guards the disk that holds the cache.
    let spec = mirage_cli::commands::volume::VolumeSpec {
        floor_bytes: spec.floor_bytes.or_else(|| {
            mirage_cli::commands::volume::cache_disk_info(spec.cache_disk.as_deref())
                .ok()
                .map(|disk| mirage_cli::commands::volume::default_floor_bytes(&disk))
        }),
        ..spec
    };
    std::thread::spawn({
        let shared = shared.clone();
        move || {
            let progress_shared = shared.clone();
            let mut progress = move |step: &str| {
                if let Ok(mut state) = progress_shared.lock() {
                    state.create["step"] = serde_json::json!(step);
                }
            };
            let credentials = mirage_cli::commands::backend_login::oauth_client_credentials(None);
            let result = credentials.and_then(|credentials| {
                mirage_cli::commands::volume::create(&spec, &credentials, None, &mut progress, None)
            });
            if let Ok(mut state) = shared.lock() {
                state.create = match result {
                    Ok(created) => serde_json::json!({
                        "in_flight": false,
                        "done": true,
                        "repository_id": created.repository_id.to_string(),
                        "drive_letter": created.drive_letter,
                        "name": created.name,
                        "budget_bytes": created.budget_bytes,
                        "cache_root": created.cache_root,
                        "account_id": created.account_id,
                    }),
                    Err(error) => serde_json::json!({
                        "in_flight": false,
                        "done": false,
                        "error": error.to_string(),
                    }),
                };
            }
        }
    });
    Ok(serde_json::json!({"started": true}))
}

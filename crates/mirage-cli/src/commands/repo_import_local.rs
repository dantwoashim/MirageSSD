use std::{path::Path, sync::Arc};

use mirage_crypto::{
    aead::RepositoryKey,
    dpapi::ProtectionScope,
    repository_key_store::{load_repository_key, save_repository_key},
};
use mirage_manifest::{ClassificationRuleSet, InventoryEntryKind, InventoryScanner};
use mirage_pack::{ImportPlan, PackEncryption, PlannedFile, import_local};
use mirage_types::{GenerationId, MirageError, RepositoryId};

use crate::output;

#[allow(clippy::too_many_arguments)]
pub fn run(
    local_only: bool,
    source: &Path,
    destination: &Path,
    repository_id: RepositoryId,
    generation_id: GenerationId,
    page_size: u32,
    pack_target: u64,
    virtual_extensions: &[String],
    minimum_virtual_asset_bytes: u64,
    unencrypted: bool,
    json: bool,
) -> Result<(), MirageError> {
    if !local_only {
        return Err(MirageError::invalid_argument(
            "repo import requires --local-only during this roadmap milestone",
        ));
    }
    let inventory = InventoryScanner::default().scan(source)?;
    let rules = classification_rules(virtual_extensions, minimum_virtual_asset_bytes)?;
    let files = inventory
        .entries
        .iter()
        .filter(|entry| entry.kind == InventoryEntryKind::File)
        .map(|entry| {
            let verdict = rules.classify(entry);
            PlannedFile {
                relative_path: entry.relative_path.clone(),
                class: verdict.class,
            }
        })
        .collect();
    let encryption = if unencrypted {
        None
    } else {
        std::fs::create_dir_all(destination).map_err(MirageError::from)?;
        let key_path = destination.join("repository-key.dpapi");
        let key = if key_path.exists() {
            load_repository_key(&key_path, repository_id)?
        } else {
            let has_packs = std::fs::read_dir(destination)
                .map_err(MirageError::from)?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("pack-") && name.ends_with(".bin"))
                });
            if has_packs {
                return Err(MirageError::repository_conflict(
                    "encrypted import key is missing for existing packs",
                ));
            }
            let key = RepositoryKey::generate()?;
            save_repository_key(
                &key_path,
                repository_id,
                &key,
                ProtectionScope::LocalMachine,
            )?;
            key
        };
        Some(PackEncryption {
            repository_id,
            key: Arc::new(key),
        })
    };
    let imported = import_local(&ImportPlan {
        repository_id,
        generation_id,
        source_root: source.to_path_buf(),
        files,
        page_size,
        pack_target,
        output_staging_directory: destination.to_path_buf(),
        encryption,
    })?;
    if json {
        output::emit_success(&serde_json::json!({
            "report_version": 1,
            "manifest_path": imported.manifest_path,
            "pack_count": imported.report.pack_count,
            "logical_bytes": imported.report.logical_bytes,
            "virtual_bytes": imported.report.virtual_bytes,
            "unique_page_bytes": imported.report.unique_page_bytes,
            "encrypted": !unencrypted,
        }))
    } else {
        println!(
            "local import complete: {} packs, {} logical bytes",
            imported.report.pack_count, imported.report.logical_bytes
        );
        Ok(())
    }
}

fn classification_rules(
    extensions: &[String],
    minimum_bytes: u64,
) -> Result<ClassificationRuleSet, MirageError> {
    if minimum_bytes == 0 || extensions.len() > 64 {
        return Err(MirageError::invalid_argument(
            "virtual asset classification policy is out of bounds",
        ));
    }
    let mut rules =
        ClassificationRuleSet::default().with_minimum_virtual_asset_bytes(minimum_bytes);
    for extension in extensions {
        let normalized = extension.trim_start_matches('.');
        if normalized.is_empty()
            || normalized.len() > 16
            || !normalized.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(MirageError::invalid_argument(
                "virtual asset extensions must be 1-16 ASCII alphanumeric characters",
            ));
        }
        rules = rules.with_virtual_asset_extension(normalized);
    }
    Ok(rules)
}

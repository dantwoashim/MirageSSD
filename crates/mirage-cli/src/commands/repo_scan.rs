use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use mirage_manifest::{ClassificationRuleSet, Inventory, InventoryScanner};
use mirage_types::{MirageError, MirageErrorKind};
use serde::Serialize;

use crate::output;

#[derive(Debug, Serialize)]
struct ScanReport {
    report_version: u32,
    virtual_extensions: Vec<String>,
    minimum_virtual_asset_bytes: u64,
    inventory: Inventory,
    classifications: Vec<ClassifiedPath>,
}

#[derive(Debug, Serialize)]
struct ClassifiedPath {
    path: String,
    verdict: mirage_manifest::ClassificationVerdict,
}

pub fn run(
    root: &Path,
    report: &Path,
    virtual_extensions: &[String],
    minimum_virtual_asset_bytes: u64,
    json: bool,
) -> Result<(), MirageError> {
    ensure_report_outside_source(root, report)?;
    let inventory = InventoryScanner::default().scan(root)?;
    if minimum_virtual_asset_bytes == 0 || virtual_extensions.len() > 64 {
        return Err(MirageError::invalid_argument(
            "virtual asset classification policy is out of bounds",
        ));
    }
    let mut normalized_extensions = Vec::with_capacity(virtual_extensions.len());
    let mut rules = ClassificationRuleSet::default()
        .with_minimum_virtual_asset_bytes(minimum_virtual_asset_bytes);
    for extension in virtual_extensions {
        let normalized = extension.trim_start_matches('.').to_ascii_lowercase();
        if normalized.is_empty()
            || normalized.len() > 16
            || !normalized.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(MirageError::invalid_argument(
                "virtual asset extensions must be 1-16 ASCII alphanumeric characters",
            ));
        }
        rules = rules.with_virtual_asset_extension(&normalized);
        normalized_extensions.push(normalized);
    }
    normalized_extensions.sort();
    normalized_extensions.dedup();
    let classifications = inventory
        .entries
        .iter()
        .filter(|entry| !entry.relative_path.is_empty())
        .map(|entry| ClassifiedPath {
            path: entry.relative_path.clone(),
            verdict: rules.classify(entry),
        })
        .collect();
    let scan_report = ScanReport {
        report_version: 1,
        virtual_extensions: normalized_extensions,
        minimum_virtual_asset_bytes,
        inventory,
        classifications,
    };
    write_report_atomic(report, &scan_report)?;
    if json {
        output::emit_success(&serde_json::json!({
            "report_version": 1,
            "report_written": true,
            "entry_count": scan_report.inventory.entries.len(),
        }))
    } else {
        println!(
            "inventory report written ({} entries)",
            scan_report.inventory.entries.len()
        );
        Ok(())
    }
}

fn ensure_report_outside_source(root: &Path, report: &Path) -> Result<(), MirageError> {
    let root = root.canonicalize().map_err(MirageError::from)?;
    let parent = report.parent().unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(MirageError::from)?;
    let file_name = report
        .file_name()
        .ok_or_else(|| MirageError::invalid_argument("report path has no file name"))?;
    let absolute_report = parent.join(file_name);
    if absolute_report.starts_with(root) {
        return Err(MirageError::invalid_argument(
            "inventory report must be outside the scanned source tree",
        ));
    }
    Ok(())
}

fn write_report_atomic(report: &Path, value: &impl Serialize) -> Result<(), MirageError> {
    let temporary = temporary_report_path(report)?;
    let result = (|| {
        let bytes = serde_json::to_vec(value).map_err(|error| {
            MirageError::internal_invariant("inventory report serialization failed")
                .with_source(error)
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(MirageError::from)?;
        file.write_all(&bytes).map_err(MirageError::from)?;
        file.sync_all().map_err(MirageError::from)?;
        drop(file);
        std::fs::rename(&temporary, report).map_err(MirageError::from)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn temporary_report_path(report: &Path) -> Result<PathBuf, MirageError> {
    let name = report
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MirageError::invalid_argument("report file name must be valid UTF-8"))?;
    let parent = report.parent().unwrap_or_else(|| Path::new("."));
    for suffix in 0_u16..=u16::MAX {
        let candidate = parent.join(format!(".{name}.mirage-tmp-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "no temporary report name is available",
    ))
}

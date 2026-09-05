use mirage_types::{MirageError, PageHash};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogicalPage {
    pub file_id: [u8; 16],
    pub offset: u64,
    pub length: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionPage {
    pub logical: LogicalPage,
    pub hash: PageHash,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    ExactHash,
    LogicalRemap,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferredPage {
    pub old: VersionPage,
    pub new: VersionPage,
    pub kind: TransferKind,
    pub confidence_millionths: u32,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferReport {
    pub pages: Vec<TransferredPage>,
    pub retained_bytes: u64,
    pub remapped_bytes: u64,
    pub invalidated_bytes: u64,
}

pub fn transfer(
    old: &[VersionPage],
    new: &[VersionPage],
    allow_logical_remap: bool,
    logical_confidence_millionths: u32,
) -> Result<TransferReport, MirageError> {
    if logical_confidence_millionths > 1_000_000 {
        return Err(MirageError::invalid_argument(
            "version transfer confidence exceeds one",
        ));
    }
    let by_hash: BTreeMap<_, _> = new.iter().map(|p| (p.hash, *p)).collect();
    let by_logical: BTreeMap<_, _> = new.iter().map(|p| (p.logical, *p)).collect();
    let mut report = TransferReport::default();
    for &prior in old {
        if let Some(&current) = by_hash.get(&prior.hash) {
            report.retained_bytes = report
                .retained_bytes
                .saturating_add(prior.logical.length as u64);
            report.pages.push(TransferredPage {
                old: prior,
                new: current,
                kind: TransferKind::ExactHash,
                confidence_millionths: 1_000_000,
            });
        } else if allow_logical_remap {
            if let Some(&current) = by_logical.get(&prior.logical) {
                report.remapped_bytes = report
                    .remapped_bytes
                    .saturating_add(prior.logical.length as u64);
                report.pages.push(TransferredPage {
                    old: prior,
                    new: current,
                    kind: TransferKind::LogicalRemap,
                    confidence_millionths: logical_confidence_millionths,
                });
            } else {
                report.invalidated_bytes = report
                    .invalidated_bytes
                    .saturating_add(prior.logical.length as u64)
            }
        } else {
            report.invalidated_bytes = report
                .invalidated_bytes
                .saturating_add(prior.logical.length as u64)
        }
    }
    report.pages.sort_by_key(|p| p.old.logical);
    Ok(report)
}

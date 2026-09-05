mod activate;
mod build_manifest;
mod context;
mod native_snapshot;
mod overlay;
mod stage;
mod stage_upload;
pub use activate::{
    ActivationBackend, ActivationPlan, ActivationReport, GenerationMounter, activate_generation,
};
pub use build_manifest::{ManifestOverlay, build_updated_manifest};
pub use context::UpdateContext;
pub use native_snapshot::{
    NativeSnapshotRecord, create_native_snapshots, restore_native_snapshots,
};
pub use overlay::{BasePageSource, OverlayJournal, OverlayMutation, OverlayStore, WriteOutcome};
pub use stage::{DirtyPageSnapshot, DirtyPageSource, StagedPack, stage_stable_pages};
pub use stage_upload::{StagedPageMapping, StagingCatalog, upload_staged_pack};

//! Exact profile/build/configuration binding (B08): a profile applies only to
//! the repository, manifest, schema, and configuration it was captured under.

use mirage_types::{ManifestHash, MirageError, RepositoryId};

use crate::profile::GameProfile;

/// Profile format version understood by this build.
pub const PROFILE_FORMAT_VERSION: u32 = 1;

/// Identity a stored profile must match exactly before its observations resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileBinding {
    pub repository_id: RepositoryId,
    pub manifest_hash: ManifestHash,
    pub format_version: u32,
    pub label: String,
}

/// Why a profile failed binding. Checked in schema, repository, manifest, label order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRejection {
    WrongRepository,
    WrongManifest,
    SchemaMismatch,
    ConfigurationMismatch,
}

impl BindingRejection {
    /// Stable lowercase identifier for diagnostics and durable adapters.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WrongRepository => "wrong_repository",
            Self::WrongManifest => "wrong_manifest",
            Self::SchemaMismatch => "schema_mismatch",
            Self::ConfigurationMismatch => "configuration_mismatch",
        }
    }
}

/// Checks a profile against the binding. Returns the first rejection in
/// schema, repository, manifest, label order.
pub fn check_binding(
    profile: &GameProfile,
    binding: &ProfileBinding,
) -> Result<(), BindingRejection> {
    if profile.format_version != binding.format_version {
        return Err(BindingRejection::SchemaMismatch);
    }
    if profile.repository_id != binding.repository_id {
        return Err(BindingRejection::WrongRepository);
    }
    if profile.manifest_hash != binding.manifest_hash {
        return Err(BindingRejection::WrongManifest);
    }
    if profile.label != binding.label {
        return Err(BindingRejection::ConfigurationMismatch);
    }
    Ok(())
}

/// Fails closed: a rejected profile becomes an error whose message names the rejection.
pub fn bind_profile(profile: &GameProfile, binding: &ProfileBinding) -> Result<(), MirageError> {
    check_binding(profile, binding).map_err(|rejection| match rejection {
        BindingRejection::SchemaMismatch => MirageError::unsupported_layout(format!(
            "profile binding rejected: {}",
            rejection.as_str()
        )),
        BindingRejection::WrongRepository
        | BindingRejection::WrongManifest
        | BindingRejection::ConfigurationMismatch => MirageError::repository_conflict(format!(
            "profile binding rejected: {}",
            rejection.as_str()
        )),
    })
}

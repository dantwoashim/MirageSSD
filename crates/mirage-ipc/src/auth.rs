use crate::Command;
use mirage_types::MirageError;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalRole {
    RepositoryOwner,
    Administrator,
    Service,
    ReadOnly,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub windows_sid: String,
    pub role: PrincipalRole,
    pub authenticated: bool,
}
pub struct Authorization;
impl Authorization {
    pub fn authenticate(principal: &Principal) -> Result<(), MirageError> {
        if !principal.authenticated
            || principal.windows_sid.is_empty()
            || principal.windows_sid.len() > 256
        {
            return Err(MirageError::backend_unauthenticated(
                "IPC client identity is not authenticated",
            ));
        }
        Ok(())
    }

    pub fn authorize(principal: &Principal, command: &Command) -> Result<(), MirageError> {
        Self::authenticate(principal)?;
        let allowed = match principal.role {
            PrincipalRole::Service | PrincipalRole::Administrator => true,
            PrincipalRole::RepositoryOwner => true,
            PrincipalRole::ReadOnly => !command.mutates(),
        };
        if allowed {
            Ok(())
        } else {
            Err(MirageError::backend_permission_denied(
                "IPC command is not authorized",
            ))
        }
    }

    pub fn authorize_repository(
        principal: &Principal,
        command: &Command,
        repository_owner_sid: &str,
    ) -> Result<(), MirageError> {
        Self::authenticate(principal)?;
        match principal.role {
            PrincipalRole::Service | PrincipalRole::Administrator => Ok(()),
            PrincipalRole::RepositoryOwner if principal.windows_sid == repository_owner_sid => {
                Ok(())
            }
            PrincipalRole::ReadOnly
                if principal.windows_sid == repository_owner_sid && !command.mutates() =>
            {
                Ok(())
            }
            PrincipalRole::RepositoryOwner | PrincipalRole::ReadOnly => {
                Err(MirageError::backend_permission_denied(
                    "IPC principal does not own this repository",
                ))
            }
        }
    }
}

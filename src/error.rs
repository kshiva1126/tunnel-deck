use thiserror::Error;

/// Errors that may reach the process entry point.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("{feature} is not implemented yet")]
    Unavailable { feature: &'static str },

    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("SSH host operation failed: {0}")]
    Host(#[from] crate::application::hosts::HostError),

    #[error("SSH connection test failed: {0}")]
    Connection(String),
}

/// Stable process exit statuses used by scripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExitStatus {
    Success = 0,
    Usage = 2,
    Unavailable = 3,
    Software = 70,
}

impl From<&AppError> for ExitStatus {
    fn from(error: &AppError) -> Self {
        match error {
            AppError::Unavailable { .. } => Self::Unavailable,
            AppError::Host(crate::application::hosts::HostError::InvalidAlias(_)) => Self::Usage,
            AppError::Configuration(_)
            | AppError::Ipc(_)
            | AppError::Host(_)
            | AppError::Connection(_) => Self::Software,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppError, ExitStatus};
    use crate::{application::hosts::HostError, domain::host::HostAlias};

    #[test]
    fn unavailable_has_stable_exit_status() {
        let error = AppError::Unavailable { feature: "TUI" };
        assert_eq!(ExitStatus::from(&error), ExitStatus::Unavailable);
        assert_eq!(ExitStatus::Unavailable as u8, 3);
    }

    #[test]
    fn invalid_host_alias_has_usage_exit_status() {
        let validation = HostAlias::new("not an alias").unwrap_err();
        let error = AppError::Host(HostError::InvalidAlias(validation));
        assert_eq!(ExitStatus::from(&error), ExitStatus::Usage);
        assert_eq!(ExitStatus::Usage as u8, 2);
    }
}

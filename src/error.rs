use thiserror::Error;

use crate::ipc::ErrorCode;

/// Errors that may reach the process entry point.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("{feature} is not implemented yet")]
    Unavailable { feature: &'static str },

    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("daemon request failed ({code:?}): {message}")]
    Daemon { code: ErrorCode, message: String },

    #[error("{{\"error\":{{\"code\":\"{code}\",\"message\":{message}}}}}", code = error_code_name(*code), message = serde_json::to_string(message).unwrap_or_else(|_| "\"unavailable\"".to_owned()))]
    JsonDaemon { code: ErrorCode, message: String },

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
    NotFound = 4,
    Conflict = 5,
    ServiceUnavailable = 6,
    Software = 70,
}

impl From<&AppError> for ExitStatus {
    fn from(error: &AppError) -> Self {
        match error {
            AppError::Unavailable { .. } => Self::Unavailable,
            AppError::Host(crate::application::hosts::HostError::InvalidAlias(_)) => Self::Usage,
            AppError::Daemon { code, .. } | AppError::JsonDaemon { code, .. } => match code {
                ErrorCode::InvalidRequest
                | ErrorCode::MessageTooLarge
                | ErrorCode::UnsupportedVersion => Self::Usage,
                ErrorCode::NotFound => Self::NotFound,
                ErrorCode::Conflict => Self::Conflict,
                ErrorCode::Unavailable => Self::ServiceUnavailable,
                ErrorCode::Internal => Self::Software,
            },
            AppError::Configuration(_)
            | AppError::Ipc(_)
            | AppError::Host(_)
            | AppError::Connection(_) => Self::Software,
        }
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::MessageTooLarge => "message_too_large",
        ErrorCode::UnsupportedVersion => "unsupported_version",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Conflict => "conflict",
        ErrorCode::Unavailable => "unavailable",
        ErrorCode::Internal => "internal",
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

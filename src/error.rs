use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    Unsupported,
    PermissionDenied,
    DependencyMissing,
    Io,
    Provider,
    Protocol,
    Render,
    Cancelled,
    Budget,
    NotImplemented,
    Conflict,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::DependencyMissing => "dependency_missing",
            ErrorCode::Io => "io_error",
            ErrorCode::Provider => "provider_error",
            ErrorCode::Protocol => "protocol_error",
            ErrorCode::Render => "render_failed",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Budget => "budget_exceeded",
            ErrorCode::NotImplemented => "not_implemented",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Internal => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
            hint: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io, message)
    }
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Cancelled, message)
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.hint {
            Some(hint) => write!(
                f,
                "{}: {} (hint: {})",
                self.code.as_str(),
                self.message,
                hint
            ),
            None => write!(f, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(ErrorCode::Io, e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::new(ErrorCode::InvalidInput, format!("json: {e}"))
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::new(ErrorCode::Internal, format!("database: {e}"))
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::invalid(format!("config parse: {e}"))
    }
}

impl From<toml::ser::Error> for Error {
    fn from(e: toml::ser::Error) -> Self {
        Error::internal(format!("config encode: {e}"))
    }
}

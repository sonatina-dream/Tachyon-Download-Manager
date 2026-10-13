use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum EngineError {
    #[error("HTTP {status} {reason}")]
    Http {
        status: u16,
        reason: String,
        /// Seconds from a `Retry-After` header.
        retry_after: Option<u64>,
    },
    #[error("network error: {0}")]
    Network(String),
    #[error("connection timed out")]
    Timeout,
    #[error("unexpected server response: {0}")]
    Protocol(String),
    #[error("the server does not support resuming this download")]
    NotResumable,
    #[error("disk error: {0}")]
    Io(String),
    #[error("{0}")]
    Config(String),
    #[error("download not found")]
    NotFound,
    #[error("cancelled")]
    Cancelled,
}

impl EngineError {
    pub fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            return Self::Timeout;
        }
        if let Some(status) = err.status() {
            return Self::Http {
                status: status.as_u16(),
                reason: status.canonical_reason().unwrap_or("error").to_string(),
                retry_after: None,
            };
        }
        // The innermost cause carries the useful text ("connection refused", DNS errors…).
        let mut source: &dyn std::error::Error = &err;
        while let Some(next) = source.source() {
            source = next;
        }
        Self::Network(source.to_string())
    }

    /// Whether the server is asking us to slow down (rate limit or overload).
    pub fn is_throttle(&self) -> bool {
        matches!(self, Self::Http { status: 429 | 503, .. })
    }

    /// Whether retrying the same request can reasonably succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) | Self::Timeout => true,
            Self::Http { status, .. } => matches!(status, 408 | 425 | 429 | 500..=599),
            Self::Protocol(_) => true,
            Self::NotResumable | Self::Io(_) | Self::Config(_) | Self::NotFound | Self::Cancelled => {
                false
            }
        }
    }
}

impl From<std::io::Error> for EngineError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

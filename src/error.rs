//! Errors returned by the client.

use std::fmt;
use std::time::Duration;

/// The error type of the Balíkobot client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Arguments rejected locally before any network call.
    InvalidRequest,
    /// A permanent refusal of the request or of the supplied data.
    Rejected,
    /// The provider is temporarily unavailable or the request never left the
    /// client. A retry can succeed.
    Unavailable {
        /// The provider retry hint, when it sent one.
        retry_after: Option<Duration>,
    },
    /// A documented "no data yet" answer.
    NotFound,
    /// A mutating call may have reached the provider. Reconcile the result by
    /// external reference before any retry.
    Ambiguous,
    /// A provider answer that violates the protocol.
    InvalidResponse,
    /// An invalid client configuration.
    Config(String),
}

impl Error {
    /// Returns the provider retry hint of an [`Error::Unavailable`], if any.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Unavailable { retry_after } => *retry_after,
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => f.write_str("balikobot: invalid request"),
            Self::Rejected => f.write_str("balikobot: provider permanently rejected the request"),
            Self::Unavailable { retry_after: None } => {
                f.write_str("balikobot: temporarily unavailable")
            }
            Self::Unavailable {
                retry_after: Some(hint),
            } => write!(
                f,
                "balikobot: temporarily unavailable (retry after {hint:?})"
            ),
            Self::NotFound => f.write_str("balikobot: resource not found"),
            Self::Ambiguous => f.write_str("balikobot: request outcome is unknown"),
            Self::InvalidResponse => f.write_str("balikobot: invalid provider response"),
            Self::Config(message) => write!(f, "balikobot: invalid configuration: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// A specialized [`std::result::Result`] for the Balíkobot client.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_follows_the_sentinels() {
        assert_eq!(
            Error::InvalidRequest.to_string(),
            "balikobot: invalid request"
        );
        assert_eq!(
            Error::Unavailable { retry_after: None }.to_string(),
            "balikobot: temporarily unavailable"
        );
        assert_eq!(
            Error::Unavailable {
                retry_after: Some(Duration::from_secs(2)),
            }
            .to_string(),
            "balikobot: temporarily unavailable (retry after 2s)"
        );
        assert_eq!(
            Error::Config("user is required".to_owned()).to_string(),
            "balikobot: invalid configuration: user is required"
        );
    }

    #[test]
    fn retry_after_only_covers_unavailable() {
        assert_eq!(
            Error::Unavailable {
                retry_after: Some(Duration::from_secs(5)),
            }
            .retry_after(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(Error::Unavailable { retry_after: None }.retry_after(), None);
        assert_eq!(Error::NotFound.retry_after(), None);
    }
}

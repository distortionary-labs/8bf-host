//! Errors reported by the command line and the server.
//!
//! Each variant maps to a documented process exit code so scripts can tell
//! failure classes apart. Messages are written for the person running the
//! command: what failed, and where possible what to do about it.

use std::path::PathBuf;

/// Process exit codes. These are part of the command line interface; see
/// the "Exit codes" section of the README before changing them.
pub mod exit {
    pub const SUCCESS: u8 = 0;
    /// The filter ran and reported an error.
    pub const FILTER_FAILED: u8 = 1;
    /// Invalid arguments (also used by clap).
    pub const USAGE: u8 = 2;
    /// No filter matches, or more than one does.
    pub const FILTER_NOT_FOUND: u8 = 3;
    /// The filter cannot run on this system with the current configuration.
    pub const CANNOT_RUN: u8 = 4;
    /// An image could not be read or written.
    pub const IMAGE_IO: u8 = 5;
    /// The configuration file is invalid.
    pub const CONFIG: u8 = 6;
    /// The run was cancelled (Ctrl+C, or the filter's own Cancel button).
    pub const CANCELLED: u8 = 130;
}

/// An error with a user-facing message and an exit code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),

    #[error("{message}")]
    FilterNotFound { message: String },

    #[error("{0}")]
    CannotRun(String),

    #[error("{message}")]
    FilterFailed { message: String, code: i16 },

    #[error("cancelled")]
    Cancelled,

    #[error("could not read {path}: {reason}")]
    ImageRead { path: PathBuf, reason: String },

    #[error("could not write {path}: {reason}")]
    ImageWrite { path: PathBuf, reason: String },

    #[error("{path}: {reason}")]
    Config { path: PathBuf, reason: String },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// The process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Usage(_) => exit::USAGE,
            Error::FilterNotFound { .. } => exit::FILTER_NOT_FOUND,
            Error::CannotRun(_) => exit::CANNOT_RUN,
            Error::FilterFailed { .. } => exit::FILTER_FAILED,
            Error::Cancelled => exit::CANCELLED,
            Error::ImageRead { .. } | Error::ImageWrite { .. } => exit::IMAGE_IO,
            Error::Config { .. } => exit::CONFIG,
            Error::Io { .. } => exit::FILTER_FAILED,
        }
    }

    /// A short machine-readable name, used in JSON output and HTTP errors.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::FilterNotFound { .. } => "filter_not_found",
            Error::CannotRun(_) => "cannot_run",
            Error::FilterFailed { .. } => "filter_failed",
            Error::Cancelled => "cancelled",
            Error::ImageRead { .. } => "image_read",
            Error::ImageWrite { .. } => "image_write",
            Error::Config { .. } => "config",
            Error::Io { .. } => "io",
        }
    }

    /// Wraps an I/O error with a description of what was being done.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_class_has_a_distinct_exit_code() {
        let codes = [
            Error::Usage(String::new()).exit_code(),
            Error::FilterNotFound {
                message: String::new(),
            }
            .exit_code(),
            Error::CannotRun(String::new()).exit_code(),
            Error::FilterFailed {
                message: String::new(),
                code: 0,
            }
            .exit_code(),
            Error::Cancelled.exit_code(),
            Error::ImageRead {
                path: PathBuf::new(),
                reason: String::new(),
            }
            .exit_code(),
            Error::Config {
                path: PathBuf::new(),
                reason: String::new(),
            }
            .exit_code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
        assert!(!codes.contains(&exit::SUCCESS));
    }
}

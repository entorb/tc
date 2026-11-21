//! Core of tc: panel model and file operations, no UI dependency.

use std::io;
use std::path::PathBuf;

pub mod edit;
pub mod ops;
pub mod panel;
pub mod search;
pub mod view;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: already exists", .0.display())]
    Exists(PathBuf),
    #[error("{}: cannot copy or move a directory into itself", .0.display())]
    IntoItself(PathBuf),
    #[error("invalid name: {0}")]
    InvalidName(String),
    #[error("not enough free memory to edit (need {}, free {})", edit::human(*need), edit::human(*free))]
    OutOfMemory { need: u64, free: u64 },
    #[error("not a text file")]
    Binary,
    #[error("invalid pattern: {0}")]
    Pattern(String),
    #[error("cancelled")]
    Cancelled,
    #[error("text not representable in {0}, convert to UTF-8")]
    Unrepresentable(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attach the path to an io error.
pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}

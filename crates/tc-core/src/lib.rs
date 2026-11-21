//! Core of tc: panel model and file operations, no UI dependency.

use std::io;
use std::path::PathBuf;

pub mod ops;
pub mod panel;

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
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attach the path to an io error.
pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}

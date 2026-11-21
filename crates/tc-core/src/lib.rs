//! Core of tc: panel model and file operations, no UI dependency.

use std::io;
use std::path::PathBuf;

pub mod archive;
pub mod cmdline;
pub mod edit;
pub mod eta;
pub mod find;
pub mod hotlist;
pub mod ops;
pub mod panel;
pub mod search;
pub mod stats;
pub mod syntax;
pub mod vfs;
pub mod view;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: is the same file", .0.display())]
    Exists(PathBuf),
    #[error("{}: file and directory cannot replace each other", .0.display())]
    TypeMismatch(PathBuf),
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
    #[error("archive: {0}")]
    Archive(String),
    #[error("{0}")]
    Remote(String),
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

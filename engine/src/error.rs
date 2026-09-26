//! Engine error type. Deliberately coarse: the UI layers plain-English
//! wording on top; the engine carries the machine facts.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// A `.git` directory wasn't found (or required internals are missing).
    NotARepository(String),
    /// A file/object the caller asked for doesn't exist.
    NotFound(String),
    /// Bytes were present but didn't parse (corrupt or unsupported format).
    Corrupt(String),
    /// The filesystem layer failed (io message from StdFs or the browser
    /// snapshot).
    Io(String),
    /// A write-path precondition failed (e.g. ref moved underneath us).
    #[cfg(feature = "pro")]
    WriteConflict(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::NotARepository(m) => write!(f, "not a git repository: {m}"),
            GitError::NotFound(m) => write!(f, "not found: {m}"),
            GitError::Corrupt(m) => write!(f, "corrupt or unsupported: {m}"),
            GitError::Io(m) => write!(f, "filesystem error: {m}"),
            #[cfg(feature = "pro")]
            GitError::WriteConflict(m) => write!(f, "write conflict: {m}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<std::io::Error> for GitError {
    fn from(e: std::io::Error) -> Self {
        GitError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, GitError>;

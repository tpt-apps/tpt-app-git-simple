//! The filesystem abstraction the whole engine reads through.
//!
//! Paths are repo-root-relative, `/`-separated, and never start with `/`
//! (e.g. `.git/objects/ab/cdef…`, `docs/readme.md`). The two implementations:
//!
//! - [`StdFs`] — real disk, rooted at the repo checkout; used by host tests
//!   and the Pro desktop edition.
//! - [`MemoryFs`] — an in-memory map; used by unit tests and by the browser
//!   edition, where a JS walker snapshots the user-selected directory into
//!   it before any engine call.

use std::collections::BTreeMap;

use crate::error::{GitError, Result};

pub trait GitFs {
    /// Reads a file's bytes. Errors with `NotFound` when absent.
    fn read_file(&self, path: &str) -> Result<Vec<u8>>;

    /// Lists a directory's entry names (no `.`/`..`, no recursion). Errors
    /// with `NotFound` when the directory doesn't exist.
    fn list_dir(&self, path: &str) -> Result<Vec<String>>;

    /// Whether a file or directory exists at `path`.
    fn exists(&self, path: &str) -> bool;

    // --- write path (Pro only; the free build never links these) ---

    #[cfg(feature = "pro")]
    fn write_file(&self, path: &str, contents: &[u8]) -> Result<()>;
    #[cfg(feature = "pro")]
    fn create_dir_all(&self, path: &str) -> Result<()>;
    #[cfg(feature = "pro")]
    fn remove_file(&self, path: &str) -> Result<()>;
}

/// Real-disk filesystem rooted at a repository checkout directory.
pub struct StdFs {
    root: std::path::PathBuf,
}

impl StdFs {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        StdFs { root: root.into() }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> std::path::PathBuf {
        let mut p = self.root.clone();
        for part in path.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            // `..` would let a crafted path escape the root; git paths never
            // contain it, so treat it as absent rather than resolving it.
            if part == ".." {
                continue;
            }
            p.push(part);
        }
        p
    }
}

impl GitFs for StdFs {
    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let full = self.resolve(path);
        match std::fs::read(&full) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(GitError::NotFound(path.to_string()))
            }
            Err(e) => Err(GitError::Io(format!("{}: {e}", full.display()))),
        }
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let full = self.resolve(path);
        let mut names = Vec::new();
        let entries = match std::fs::read_dir(&full) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GitError::NotFound(path.to_string()))
            }
            Err(e) => return Err(GitError::Io(format!("{}: {e}", full.display()))),
        };
        for entry in entries {
            let entry = entry.map_err(|e| GitError::Io(format!("{}: {e}", full.display())))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).exists()
    }

    #[cfg(feature = "pro")]
    fn write_file(&self, path: &str, contents: &[u8]) -> Result<()> {
        let full = self.resolve(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GitError::Io(format!("{}: {e}", parent.display())))?;
        }
        // Write-then-rename so a crash mid-write can't leave a truncated
        // ref or object behind.
        let tmp = full.with_extension("tpt-tmp");
        std::fs::write(&tmp, contents).map_err(|e| GitError::Io(format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &full).map_err(|e| GitError::Io(format!("{}: {e}", full.display())))
    }

    #[cfg(feature = "pro")]
    fn create_dir_all(&self, path: &str) -> Result<()> {
        let full = self.resolve(path);
        std::fs::create_dir_all(&full).map_err(|e| GitError::Io(format!("{}: {e}", full.display())))
    }

    #[cfg(feature = "pro")]
    fn remove_file(&self, path: &str) -> Result<()> {
        let full = self.resolve(path);
        match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(GitError::Io(format!("{}: {e}", full.display()))),
        }
    }
}

/// In-memory filesystem: sorted map of path → bytes. Directories are
/// implicit (any path prefix that has children).
#[derive(Default, Clone)]
pub struct MemoryFs {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemoryFs {
    pub fn new() -> Self {
        MemoryFs { files: BTreeMap::new() }
    }

    pub fn add(&mut self, path: &str, contents: Vec<u8>) {
        self.files.insert(path.to_string(), contents);
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Total bytes held (the browser snapshot reports this in the UI).
    pub fn total_bytes(&self) -> u64 {
        self.files.values().map(|v| v.len() as u64).sum()
    }
}

impl GitFs for MemoryFs {
    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| GitError::NotFound(path.to_string()))
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut names: Vec<String> = Vec::new();
        for key in self.files.keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            match rest.find('/') {
                Some(i) => {
                    let dir = rest[..i].to_string();
                    if !names.contains(&dir) {
                        names.push(dir);
                    }
                }
                None => names.push(rest.to_string()),
            }
        }
        if names.is_empty() && !self.files.keys().any(|k| k.starts_with(&prefix)) {
            return Err(GitError::NotFound(path.to_string()));
        }
        Ok(names)
    }

    fn exists(&self, path: &str) -> bool {
        if self.files.contains_key(path) {
            return true;
        }
        let prefix = format!("{path}/");
        self.files.keys().any(|k| k.starts_with(&prefix))
    }

    #[cfg(feature = "pro")]
    fn write_file(&self, _path: &str, _contents: &[u8]) -> Result<()> {
        Err(GitError::Io(
            "MemoryFs is read-only (browser snapshots are immutable; the Pro \
             desktop edition writes through StdFs)"
                .into(),
        ))
    }

    #[cfg(feature = "pro")]
    fn create_dir_all(&self, _path: &str) -> Result<()> {
        Err(GitError::Io("MemoryFs is read-only".into()))
    }

    #[cfg(feature = "pro")]
    fn remove_file(&self, _path: &str) -> Result<()> {
        Err(GitError::Io("MemoryFs is read-only".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_fs_list_dir_groups_directories() {
        let mut fs = MemoryFs::new();
        fs.add("docs/a.md", b"A".to_vec());
        fs.add("docs/b/c.md", b"C".to_vec());
        fs.add("readme.md", b"R".to_vec());
        let root = GitFs::list_dir(&fs, "").unwrap();
        assert_eq!(root, vec!["docs".to_string(), "readme.md".to_string()]);
        let docs = GitFs::list_dir(&fs, "docs").unwrap();
        assert_eq!(docs, vec!["a.md".to_string(), "b".to_string()]);
    }

    #[test]
    fn memory_fs_missing_dir_is_not_found() {
        let fs = MemoryFs::new();
        assert!(matches!(
            GitFs::list_dir(&fs, "nope"),
            Err(GitError::NotFound(_))
        ));
    }
}

//! Worktree-vs-HEAD status: what would a "save" pick up?
//!
//! Deliberately index-free (see the crate docs): a path is compared against
//! the HEAD tree directly. `.git/` is always skipped; `.gitignore` is *not*
//! parsed — the UI stages explicit file lists, so untracked noise is
//! informational only.

use std::collections::HashMap;

use crate::error::Result;
use crate::fs::GitFs;
use crate::oid::Oid;
use crate::repo::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// In HEAD, gone (or emptied) in the worktree.
    Deleted,
    /// Present in both, different content.
    Modified,
    /// Not in HEAD, present in the worktree.
    Untracked,
}

impl ChangeKind {
    /// The plain-English label the UI shows.
    pub fn label(&self) -> &'static str {
        match self {
            ChangeKind::Deleted => "Deleted",
            ChangeKind::Modified => "Changed",
            ChangeKind::Untracked => "New file",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub path: String,
    pub kind: ChangeKind,
    /// Blob oid at HEAD (set unless untracked).
    pub head_oid: Option<Oid>,
}

impl StatusEntry {
    /// Sort key: modified/unchanged-ish first, untracked last, each group
    /// alphabetical (matches the "what did I touch" mental model).
    pub fn sort_key(&self) -> (u8, String) {
        let group = match self.kind {
            ChangeKind::Modified => 0,
            ChangeKind::Deleted => 1,
            ChangeKind::Untracked => 2,
        };
        (group, self.path.clone())
    }
}

/// Scans the worktree against HEAD. `Ok(vec![])` on a repo with no commits.
pub fn status<F: GitFs>(repo: &mut Repo<F>) -> Result<Vec<StatusEntry>> {
    let head = repo.head()?;
    let head_map: HashMap<String, (String, Oid)> = match head.commit {
        Some(commit) => repo.tree_map(commit)?,
        None => HashMap::new(),
    };

    let mut worktree: Vec<String> = Vec::new();
    collect_worktree(repo, "", &mut worktree)?;

    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for path in &worktree {
        seen.insert(path.clone());
        let bytes = repo.fs().read_file(path)?;
        let matches_head = head_map
            .get(path)
            .map(|(_, oid)| blob_oid_matches(repo, *oid, &bytes))
            .unwrap_or(false);
        if !matches_head {
            let kind = if head_map.contains_key(path) {
                ChangeKind::Modified
            } else {
                ChangeKind::Untracked
            };
            entries.push(StatusEntry {
                path: path.clone(),
                kind,
                head_oid: head_map.get(path).map(|(_, oid)| *oid),
            });
        }
    }
    for (path, (_, oid)) in &head_map {
        if !seen.contains(path) {
            entries.push(StatusEntry {
                path: path.clone(),
                kind: ChangeKind::Deleted,
                head_oid: Some(*oid),
            });
        }
    }

    entries.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    Ok(entries)
}

/// A worktree file counts as unchanged only if its bytes hash to the same
/// blob oid HEAD has (content compare via the object id, exactly git's own
/// test).
fn blob_oid_matches<F: GitFs>(repo: &mut Repo<F>, head_blob: Oid, bytes: &[u8]) -> bool {
    let computed = crate::objects::blob_oid(bytes);
    if computed == head_blob {
        return true;
    }
    // Same bytes can legitimately live under a different oid only via
    // filters (line endings); do a byte-level read as the fallback so a
    // CRLF-smudged checkout isn't permanently "modified".
    match repo.blob(&head_blob) {
        Ok(head_bytes) => head_bytes == bytes,
        Err(_) => false,
    }
}

fn collect_worktree<F: GitFs>(repo: &Repo<F>, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let names = match repo.fs().list_dir(prefix) {
        Ok(names) => names,
        // A missing dir contributes nothing; an Io error means list_dir hit
        // a *file* path — also not a directory, so nothing to descend.
        Err(crate::error::GitError::NotFound(_)) | Err(crate::error::GitError::Io(_)) => {
            return Ok(())
        }
        Err(e) => return Err(e),
    };
    for name in names {
        if prefix.is_empty() && name == ".git" {
            continue;
        }
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        match repo.fs().list_dir(&path) {
            Ok(_) => collect_worktree(repo, &path, out)?,
            Err(crate::error::GitError::NotFound(_))
            | Err(crate::error::GitError::Io(_)) => out.push(path),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

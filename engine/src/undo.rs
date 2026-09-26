//! Safe undo (pro).
//!
//! Every destructive-feeling action leaves a real recovery path before it
//! touches anything:
//!
//! - **Undo last save** moves the branch ref to the commit's first parent
//!   (soft — files keep their content) and appends a reflog entry. Undoing
//!   the undo moves the ref back; the reflog is the record.
//! - **Discard changes** writes the current worktree bytes into fresh
//!   blobs, wraps them in a backup commit, and parks it under
//!   `refs/tpt-git-simple/backups/<timestamp>-<n>` — an ordinary ref git
//!   won't garbage-collect while it exists — then restores the file from
//!   HEAD. "Restore discarded changes" re-applies a backup's blobs to the
//!   worktree.

use crate::commit::{update_branch_ref, validate_path, Identity};
use crate::error::{GitError, Result};
use crate::fs::GitFs;
use crate::objects::{render_signature, ObjectType, Signature};
use crate::oid::Oid;
use crate::repo::Repo;

/// One recoverable discard operation.
#[derive(Debug, Clone)]
pub struct BackupRef {
    /// Full ref name, e.g. `refs/tpt-git-simple/backups/1727310000-1`.
    pub ref_name: String,
    pub commit: Oid,
    pub when: i64,
    /// Paths whose pre-discard contents the backup holds.
    pub paths: Vec<String>,
}

/// The number of discard-backups the UI offers before the oldest becomes a
/// pure reflog footnote (still recoverable until gc — but not listed).
pub const MAX_LISTED_BACKUPS: usize = 20;

/// Undo the last save on the current branch: move the ref to the commit's
/// first parent, keeping every file as-is (git's `reset --soft`, plus a
/// reflog trail). Returns the oid the branch now points at.
pub fn undo_last_save<F: GitFs>(
    repo: &mut Repo<F>,
    identity: &Identity,
) -> Result<Oid> {
    let head = repo.head()?;
    let Some(current) = head.commit else {
        return Err(GitError::NotFound("nothing saved yet".into()));
    };
    let Some(branch) = head.branch else {
        return Err(GitError::WriteConflict("HEAD is detached — nothing to undo onto".into()));
    };
    let commit = repo.commit(&current)?;
    let Some(parent) = commit.parents.first() else {
        return Err(GitError::WriteConflict(
            "this is the repository's first save — there's nothing before it to go back to"
                .into(),
        ));
    };
    let target = *parent;
    update_branch_ref(
        repo,
        &branch,
        current,
        target,
        "tpt-git-simple: undo last save (files kept)",
    )?;
    let _ = identity; // reserved: stamped undo commits would use this
    Ok(target)
}

/// Re-applies the last undo (moves the branch ref back to the child commit
/// the reflog recorded). This is a convenience for "undo the undo"; the
/// general history is in `.git/logs/HEAD` either way.
pub fn redo_last_undo<F: GitFs>(repo: &mut Repo<F>) -> Result<Oid> {
    let head = repo.head()?;
    let Some(current) = head.commit else {
        return Err(GitError::NotFound("no current save to move".into()));
    };
    let Some(branch) = head.branch else {
        return Err(GitError::WriteConflict("HEAD is detached".into()));
    };
    // Find the newest reflog entry that moved the ref TO the current value
    // while undoing (its `new` is where we are, its `old` is where to go
    // back).
    let log = repo
        .fs()
        .read_file(&format!(".git/logs/refs/heads/{branch}"))
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&log);
    let mut target: Option<Oid> = None;
    for line in text.lines().rev() {
        let mut parts = line.splitn(3, ' ');
        let old = parts.next().and_then(Oid::from_hex);
        let new = parts.next().and_then(Oid::from_hex);
        let message = parts.next().unwrap_or("");
        if let (Some(old), Some(new)) = (old, new) {
            if new == current && message.contains("undo last save") {
                target = Some(old);
                break;
            }
        }
    }
    let Some(target) = target else {
        return Err(GitError::NotFound("no undo found to redo".into()));
    };
    update_branch_ref(repo, &branch, current, target, "tpt-git-simple: redo undone save")?;
    Ok(target)
}

/// Backs up `discard` (path → current worktree bytes) into a backup commit
/// whose tree holds the files themselves, parked under
/// `refs/tpt-git-simple/backups/<timestamp>-<seq>` — an ordinary ref git
/// won't garbage-collect while it exists. Restore reads the tree back.
pub fn backup_before_discard<F: GitFs>(
    repo: &mut Repo<F>,
    identity: &Identity,
    discard: &[(String, Vec<u8>)],
) -> Result<BackupRef> {
    if discard.is_empty() {
        return Err(GitError::WriteConflict("nothing to back up".into()));
    }
    for (path, _) in discard {
        validate_path(path)?;
    }

    let mut files: Vec<(String, Oid)> = Vec::with_capacity(discard.len());
    let mut paths = Vec::with_capacity(discard.len());
    for (path, bytes) in discard {
        let blob = crate::commit::write_object(repo, ObjectType::Blob, bytes)?;
        files.push((path.clone(), blob));
        paths.push(path.clone());
    }
    let tree = crate::commit::write_tree_from_paths(repo, &files)?;

    let head = repo.head()?;
    let now = now_unix();
    let sig = Signature {
        name: identity.name.clone(),
        email: identity.email.clone(),
        time: now,
        timezone: "+0000".into(),
    };
    let mut payload = format!("tree {}\n", tree.to_hex()).into_bytes();
    if let Some(parent) = head.commit {
        payload.extend_from_slice(format!("parent {}\n", parent.to_hex()).as_bytes());
    }
    payload.extend_from_slice(format!("author {}\n", render_signature(&sig)).as_bytes());
    payload.extend_from_slice(format!("committer {}\n", render_signature(&sig)).as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(
        format!("tpt-git-simple backup of discarded changes\n\nFiles:\n{}\n", paths.join("\n"))
            .as_bytes(),
    );
    let commit = crate::commit::write_object(repo, ObjectType::Commit, &payload)?;

    let seq = backup_sequence(repo)?;
    let ref_name = format!("refs/tpt-git-simple/backups/{now}-{seq}");
    repo.fs().write_file(
        &format!(".git/{ref_name}"),
        format!("{}\n", commit.to_hex()).as_bytes(),
    )?;
    prune_old_backups(repo)?;
    Ok(BackupRef { ref_name, commit, when: now, paths })
}

/// Lists discard backups, newest first.
pub fn list_backups<F: GitFs>(repo: &mut Repo<F>) -> Result<Vec<BackupRef>> {
    let mut out = Vec::new();
    let names = match repo.fs().list_dir(".git/refs/tpt-git-simple/backups") {
        Ok(names) => names,
        Err(GitError::NotFound(_)) => return Ok(out),
        Err(e) => return Err(e),
    };
    for name in names {
        let bytes = repo
            .fs()
            .read_file(&format!(".git/refs/tpt-git-simple/backups/{name}"))
            .unwrap_or_default();
        let Some(commit) = Oid::from_hex(String::from_utf8_lossy(&bytes).trim()) else {
            continue;
        };
        let when = name.split('-').next().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
        let paths = repo
            .commit(&commit)
            .ok()
            .map(|c| parse_backup_paths(&c.message))
            .unwrap_or_default();
        out.push(BackupRef {
            ref_name: format!("refs/tpt-git-simple/backups/{name}"),
            commit,
            when,
            paths,
        });
    }
    out.sort_by(|a, b| b.when.cmp(&a.when));
    Ok(out)
}

fn parse_backup_paths(message: &str) -> Vec<String> {
    let mut in_files = false;
    message
        .lines()
        .filter_map(|line| {
            if line == "Files:" {
                in_files = true;
                return None;
            }
            if in_files {
                if line.is_empty() {
                    in_files = false;
                    return None;
                }
                return Some(line.to_string());
            }
            None
        })
        .collect()
}

/// Restores a backup's files: reads the backup commit's tree back as
/// `path → bytes`, ready for the UI to offer "put these back".
pub fn restore_backup_contents<F: GitFs>(
    repo: &mut Repo<F>,
    backup: &BackupRef,
) -> Result<Vec<(String, Vec<u8>)>> {
    let commit = repo.commit(&backup.commit)?;
    let files = repo.list_tree_recursive(&commit.tree)?;
    let mut out = Vec::new();
    for (path, _mode, oid) in files {
        out.push((path, repo.blob(&oid)?));
    }
    Ok(out)
}

fn backup_sequence<F: GitFs>(repo: &Repo<F>) -> Result<u64> {
    let names = repo
        .fs()
        .list_dir(".git/refs/tpt-git-simple/backups")
        .unwrap_or_default();
    let seq = names
        .iter()
        .filter_map(|n| n.rsplit('-').next().and_then(|s| s.parse::<u64>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    Ok(seq)
}

fn prune_old_backups<F: GitFs>(repo: &Repo<F>) -> Result<()> {
    let mut names = repo
        .fs()
        .list_dir(".git/refs/tpt-git-simple/backups")
        .unwrap_or_default();
    if names.len() <= MAX_LISTED_BACKUPS {
        return Ok(());
    }
    names.sort();
    let excess = names.len() - MAX_LISTED_BACKUPS;
    for name in names.into_iter().take(excess) {
        let _ = repo
            .fs()
            .remove_file(&format!(".git/refs/tpt-git-simple/backups/{name}"));
    }
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Discarding worktree changes for real: `restore` is path → HEAD blob
/// bytes (empty = delete the file). Called only after
/// [`backup_before_discard`] succeeded.
pub fn apply_discard<F: GitFs>(
    repo: &Repo<F>,
    restore: &[(String, Option<Vec<u8>>)],
) -> Result<()> {
    for (path, contents) in restore {
        validate_path(path)?;
        match contents {
            Some(bytes) => repo.fs().write_file(path, bytes)?,
            None => repo.fs().remove_file(path)?,
        }
    }
    Ok(())
}

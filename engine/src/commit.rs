//! Object writing + the "save" flow (pro).
//!
//! Staging is an explicit file list — the radically-simplified semantic the
//! UI promises: "these are the files I want to save". Each staged path is
//! written as a blob from the current worktree bytes (or dropped from the
//! tree when deleted), unchanged subtrees are reused, one commit lands, and
//! both the branch ref and HEAD's reflog get an entry so undo always has a
//! trail.

use crate::error::{GitError, Result};
use crate::fs::GitFs;
use crate::objects::{render_signature, ObjectType, Signature, TreeEntry};
use crate::oid::Oid;
use crate::repo::Repo;

pub use crate::objects::blob_oid;

/// Identity used for new commits (the desktop shell collects this once in
/// Settings; the engine never guesses).
#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// One file the user chose to save. `contents: None` marks a deletion.
#[derive(Debug, Clone)]
pub struct StagedFile {
    pub path: String,
    pub contents: Option<Vec<u8>>,
}

/// Validates a path is repo-relative and safe (no `.git`, no `..`, no
/// absolute or backslash separators).
pub fn validate_path(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains(':')
        || path.split('/').any(|part| part == ".." || part == ".git" || part.is_empty());
    if bad {
        return Err(GitError::Io(format!("unsafe path: {path:?}")));
    }
    Ok(())
}

/// Writes a blob, tree, or commit object as a loose object. Existing
/// objects are left alone (same oid = same bytes, always).
pub fn write_object<F: GitFs>(
    repo: &Repo<F>,
    kind: ObjectType,
    payload: &[u8],
) -> Result<Oid> {
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(format!("{} {}\0", kind.name(), payload.len()).as_bytes());
    hasher.update(payload);
    let oid = Oid(hasher.digest().bytes());

    let path = format!(".git/objects/{}", oid.loose_path());
    if repo.fs().exists(&path) {
        return Ok(oid);
    }
    let compressed = crate::zlib::deflate(&framed(kind, payload));
    repo.fs().write_file(&path, &compressed)?;
    Ok(oid)
}

fn framed(kind: ObjectType, payload: &[u8]) -> Vec<u8> {
    let mut framed = format!("{} {}\0", kind.name(), payload.len()).into_bytes();
    framed.extend_from_slice(payload);
    framed
}

/// Builds (or reuses) the tree chain for one directory level from its
/// entries. `entries` must already be sorted by name.


/// The save flow: stage `files` on top of HEAD's tree and commit.
///
/// Returns the new commit oid. The ref update is plain (the engine has no
/// lock file protocol); callers doing concurrent writes isn't a supported
/// scenario for a single-user desktop app, and the reflog keeps every prior
/// value recoverable regardless.
pub fn commit_staged<F: GitFs>(
    repo: &mut Repo<F>,
    identity: &Identity,
    message: &str,
    files: &[StagedFile],
) -> Result<Oid> {
    for file in files {
        validate_path(&file.path)?;
    }
    if message.trim().is_empty() {
        return Err(GitError::WriteConflict("a save needs a short description".into()));
    }

    let head = repo.head()?;
    let head_commit = head
        .commit
        .ok_or_else(|| GitError::WriteConflict("saving on an empty repo isn't supported yet".into()))?;
    let branch = head
        .branch
        .ok_or_else(|| GitError::WriteConflict("HEAD is detached — no branch to save onto".into()))?;

    // Start from HEAD's tree as path → oid, apply staged files (deletes
    // drop paths), and rebuild the nested trees from the flat list.
    let head_map = repo.tree_map(head_commit)?; // path → (mode, oid)
    let mut flat: Vec<(String, Oid)> = head_map
        .iter()
        .filter(|(p, _)| !files.iter().any(|f| &f.path == *p))
        .map(|(p, (_, oid))| (p.clone(), *oid))
        .collect();
    for file in files {
        if let Some(bytes) = &file.contents {
            let blob = write_object(repo, ObjectType::Blob, bytes)?;
            flat.push((file.path.clone(), blob));
        }
    }
    // A staged deletion can empty a directory entirely; write_tree_from_paths
    // drops it naturally because no path remains under it.
    flat.sort();

    let new_tree = write_tree_from_paths(repo, &flat)?;

    // Commit object: parent = HEAD.
    let now = now_unix();
    let author = Signature {
        name: identity.name.clone(),
        email: identity.email.clone(),
        time: now,
        timezone: local_tz(now),
    };
    let mut payload = Vec::new();
    payload.extend_from_slice(format!("tree {}\n", new_tree.to_hex()).as_bytes());
    payload.extend_from_slice(format!("parent {}\n", head_commit.to_hex()).as_bytes());
    payload.extend_from_slice(format!("author {}\n", render_signature(&author)).as_bytes());
    payload.extend_from_slice(format!("committer {}\n", render_signature(&author)).as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(message.as_bytes());
    let commit_oid = write_object(repo, ObjectType::Commit, &payload)?;

    // Point the branch at the new commit + reflog.
    update_branch_ref(repo, &branch, head_commit, commit_oid, &format!("save: {}", summary(message)))?;
    Ok(commit_oid)
}

/// Builds nested trees from a flat `path → blob oid` list (all files
/// present — used by the save flow's subtree builder and by discard
/// backups). Paths are validated; duplicates collapse.
pub fn write_tree_from_paths<F: GitFs>(
    repo: &Repo<F>,
    files: &[(String, Oid)],
) -> Result<Oid> {
    for (path, _) in files {
        validate_path(path)?;
    }
    write_nested(repo, &build_level(repo, files)?)
}

/// Groups one level: top-level files become entries, subdirectories
/// recurse.
fn build_level<F: GitFs>(repo: &Repo<F>, files: &[(String, Oid)]) -> Result<Vec<TreeEntry>> {
    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut dirs: std::collections::BTreeMap<String, Vec<(String, Oid)>> =
        std::collections::BTreeMap::new();
    for (path, oid) in files {
        match path.split_once('/') {
            None => entries.push(TreeEntry {
                mode: "100644".into(),
                name: path.clone(),
                oid: *oid,
            }),
            Some((dir, rest)) => {
                dirs.entry(dir.to_string()).or_default().push((rest.to_string(), *oid));
            }
        }
    }
    for (name, sub) in dirs {
        let sub_oid = write_nested(repo, &build_level(repo, &sub)?)?;
        entries.push(TreeEntry { mode: "40000".into(), name, oid: sub_oid });
    }
    Ok(entries)
}

fn write_nested<F: GitFs>(repo: &Repo<F>, entries: &[TreeEntry]) -> Result<Oid> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(tree_entry_sort_compat);
    let mut payload = Vec::new();
    for entry in sorted {
        payload.extend_from_slice(entry.mode.as_bytes());
        payload.push(b' ');
        payload.extend_from_slice(entry.name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&entry.oid.0);
    }
    write_object(repo, ObjectType::Tree, &payload)
}

fn tree_entry_sort_compat(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    let a_key = if a.is_dir() { format!("{}/", a.name) } else { a.name.clone() };
    let b_key = if b.is_dir() { format!("{}/", b.name) } else { b.name.clone() };
    a_key.cmp(&b_key)
}


/// Updates `refs/heads/<branch>` and appends reflog entries for both the
/// branch and HEAD. Returns `(old, new)`.
pub fn update_branch_ref<F: GitFs>(
    repo: &Repo<F>,
    branch: &str,
    old: Oid,
    new: Oid,
    action: &str,
) -> Result<()> {
    validate_branch(branch)?;
    let ref_path = format!(".git/refs/heads/{branch}");
    repo.fs().write_file(&ref_path, format!("{}\n", new.to_hex()).as_bytes())?;

    append_reflog(repo, &format!(".git/logs/refs/heads/{branch}"), old, new, action)?;
    // Reflog lines are best-effort: a save must not fail because the log
    // couldn't be appended.
    let _ = append_reflog(repo, ".git/logs/HEAD", old, new, action);
    Ok(())
}

fn validate_branch(branch: &str) -> Result<()> {
    let ok = !branch.is_empty()
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.contains("..")
        && !branch.split('/').any(|p| p.is_empty() || p == ".git")
        && branch.chars().all(|c| c.is_ascii_graphic() && c != '~' && c != '^' && c != ':' && c != '?' && c != '*' && c != '[' && c != '\\');
    if !ok {
        return Err(GitError::Io(format!("unsafe branch name: {branch:?}")));
    }
    Ok(())
}

/// Appends one reflog line: `<old> <new> <name> <email> <ts> <tz>\t<msg>`.
fn append_reflog<F: GitFs>(repo: &Repo<F>, log_path: &str, old: Oid, new: Oid, action: &str) -> Result<()> {
    let (name, email) = last_identity(repo).unwrap_or_else(|| ("TPT Git Simple".into(), "git-simple@localhost".into()));
    let now = now_unix();
    let line = format!(
        "{} {} {} <{}> {} +0000\t{}\n",
        old.to_hex(),
        new.to_hex(),
        sanitize_reflog_field(&name),
        sanitize_reflog_field(&email),
        now,
        sanitize_reflog_field(action),
    );
    let mut contents = repo.fs().read_file(log_path).unwrap_or_default();
    contents.extend_from_slice(line.as_bytes());
    repo.fs().write_file(log_path, &contents)
}

fn sanitize_reflog_field(field: &str) -> String {
    field
        .chars()
        .map(|c| if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Best-effort identity from the repo's local config (fallback for callers
/// that didn't supply one).
fn last_identity<F: GitFs>(repo: &Repo<F>) -> Option<(String, String)> {
    let config = repo.fs().read_file(".git/config").ok()?;
    let text = String::from_utf8_lossy(&config);
    let (mut name, mut email) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("email = ") {
            email = Some(v.trim_matches('"').to_string());
        }
    }
    Some((name?, email?))
}

/// The engine avoids libc/localtime; git accepts `+0000` and the UI shows
/// relative times, so commits from the engine use UTC.
fn local_tz(_now: i64) -> String {
    "+0000".to_string()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn summary(message: &str) -> String {
    let first = message.lines().next().unwrap_or("");
    first.chars().take(50).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_oid_matches_git_known_vector() {
        // `echo -n "" | git hash-object --stdin` → e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
        assert_eq!(
            blob_oid(b"").to_hex(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        // "hello\n"
        assert_eq!(
            blob_oid(b"hello\n").to_hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn validates_paths() {
        assert!(validate_path("docs/a.md").is_ok());
        assert!(validate_path("../escape").is_err());
        assert!(validate_path(".git/config").is_err());
        assert!(validate_path("a\\b").is_err());
        assert!(validate_path("/abs").is_err());
    }
}

//! Native git command host for the Pro desktop shell.
//!
//! The hosted page (the same wasm UI as the free edition) can't reach the
//! real filesystem or network, so every state-changing — or network —
//! operation goes through this IPC layer. Reads also come through here on
//! the desktop (as a `git.snapshot` of path→bytes) so the UI's in-memory
//! model is identical across editions.
//!
//! Wire format: params and results are JSON; file contents are base64.
//! Every handler takes the repo `path` (picked once via the native
//! `dialog.open` capability) and re-opens the engine over `StdFs`, so
//! external edits (e.g. the user fixing conflicts in their editor) are
//! always seen fresh — the desktop side keeps no repo state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use base64::Engine as _;
use serde_json::{json, Value};
use tpt_git_engine::commit::{commit_staged_with_parents, Identity, StagedFile};
use tpt_git_engine::fs::StdFs;
use tpt_git_engine::merge::{self, MergeOutcome};
use tpt_git_engine::repo::Repo;
use tpt_git_engine::status::{self, ChangeKind};
use tpt_git_engine::undo;
use tpt_git_engine::{conflict, Oid};

/// Caps matching the browser edition's snapshot (`src/repo_source.rs`) so
/// behavior is identical wherever the app runs.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 96 * 1024 * 1024;
const MAX_FILES: usize = 8_000;
/// Fetch/push can legitimately wait on a credential prompt; past this the
/// operation is declared stuck rather than freezing the app all day.
const NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Entry point for `AppBuilder::run_with_value_handler`. Action names are
/// all under the `git.` namespace; `dialog.open` is a framework built-in.
pub fn handle(action: &str, params: &Value) -> Result<Value, String> {
    match action {
        "git.snapshot" => git_snapshot(params),
        "git.status" => git_status(params),
        "git.save" => git_save(params),
        "git.undo" => git_undo(params),
        "git.discard" => git_discard(params),
        "git.backups" => git_backups(params),
        "git.restore" => git_restore(params),
        "git.pull" => git_pull(params),
        "git.push" => git_push(params),
        other => Err(format!("unknown action {other:?}")),
    }
}

fn b64_encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::engine::general_purpose::STANDARD;
    STANDARD
        .decode(s)
        .map_err(|e| format!("bad base64 content: {e}"))
}

/// Engine errors are plain-English already; this just adapts them into the
/// IPC error type.
fn soft(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn param_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing or non-string param {key:?}"))
}

fn identity_of(params: &Value) -> Result<Identity, String> {
    Ok(Identity {
        name: param_str(params, "name")?.to_string(),
        email: param_str(params, "email")?.to_string(),
    })
}

fn repo_path(params: &Value) -> Result<PathBuf, String> {
    let path = param_str(params, "path")?;
    let path = PathBuf::from(path);
    if !path.join(".git").exists() {
        return Err(format!("{} is not a git repository", path.display()));
    }
    Ok(path)
}

/// Sets a file's bytes on disk, creating parent directories. `None`
/// deletes (missing files are fine — that's the desired end state).
fn write_worktree_file(path: &Path, rel: &str, contents: Option<&[u8]>) -> Result<(), String> {
    validate_rel(rel)?;
    let full = path.join(rel);
    match contents {
        Some(bytes) => {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&full, bytes).map_err(|e| format!("{}: {e}", full.display()))
        }
        None => match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", full.display())),
        },
    }
}

/// Same safety rule as the engine's `commit::validate_path`.
fn validate_rel(rel: &str) -> Result<(), String> {
    let bad = rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains(':')
        || rel
            .split('/')
            .any(|part| part == ".." || part == ".git" || part.is_empty());
    if bad {
        return Err(format!("unsafe path {rel:?}"));
    }
    Ok(())
}

// ----- git.snapshot -----

/// Walks the repository directory and returns every file's bytes (base64),
/// the same shape the browser edition builds from the File System Access
/// API. The UI turns this into its in-memory model.
fn git_snapshot(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut total: u64 = 0;
    walk(&root, "", &mut files, &mut skipped, &mut total, &mut truncated)?;

    let entries: Vec<Value> = files
        .into_iter()
        .map(|(p, bytes)| json!([p, b64_encode(&bytes)]))
        .collect();
    Ok(json!({
        "name": root.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".into()),
        "files": entries,
        "skipped": skipped,
        "truncated": truncated,
        "totalBytes": total,
    }))
}

fn walk(
    root: &Path,
    rel: &str,
    out: &mut Vec<(String, Vec<u8>)>,
    skipped: &mut Vec<String>,
    total: &mut u64,
    truncated: &mut bool,
) -> Result<(), String> {
    if out.len() > MAX_FILES {
        *truncated = true;
        return Ok(());
    }
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names: Vec<(String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push((name, is_dir));
    }
    names.sort();
    for (name, is_dir) in names {
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if rel.is_empty() && (name == ".git" || name == "node_modules") {
            continue;
        }
        if is_dir {
            walk(root, &child, out, skipped, total, truncated)?;
        } else if out.len() > MAX_FILES {
            *truncated = true;
            return Ok(());
        } else {
            let meta = std::fs::metadata(root.join(&child));
            let size = meta.map(|m| m.len()).unwrap_or(u64::MAX);
            if size <= MAX_FILE_BYTES && *total + size <= MAX_TOTAL_BYTES {
                match std::fs::read(root.join(&child)) {
                    Ok(bytes) => {
                        *total += bytes.len() as u64;
                        out.push((child, bytes));
                    }
                    // Unreadable (locked) files are skipped, not fatal.
                    Err(_) => skipped.push(child),
                }
            } else {
                skipped.push(child);
            }
        }
    }
    Ok(())
}

// ----- git.status -----

/// Worktree-vs-HEAD status, computed natively so it reflects the real disk
/// (the UI's snapshot can be stale after external edits, e.g. conflict
/// resolution in the user's editor).
fn git_status(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let mut repo = Repo::open(StdFs::new(root)).map_err(soft)?;
    let entries = status::status(&mut repo).map_err(soft)?;
    Ok(json!(entries
        .iter()
        .map(|e| json!({
            "path": e.path,
            "kind": match e.kind {
                ChangeKind::Deleted => "deleted",
                ChangeKind::Modified => "modified",
                ChangeKind::Untracked => "untracked",
            },
        }))
        .collect::<Vec<_>>()))
}

// ----- git.save -----

/// Saves explicitly chosen files (content = base64, or null for a deletion)
/// as one commit. `mergeParent` (optional hex oid) records a merge — the
/// conflict-resolution flow passes the fetched branch tip here.
fn git_save(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let identity = identity_of(params)?;
    let message = param_str(params, "message")?;
    let merge_parent = params.get("mergeParent").and_then(|v| v.as_str());

    let mut staged: Vec<StagedFile> = Vec::new();
    for file in params
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing files".to_string())?
    {
        let path = file
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "file missing path")?;
        let content = file.get("content").and_then(|v| v.as_str());
        staged.push(StagedFile {
            path: path.to_string(),
            contents: match content {
                None => None,
                Some(b64) => Some(b64_decode(b64)?),
            },
        });
    }

    let mut repo = Repo::open(StdFs::new(root)).map_err(soft)?;
    let extra: Vec<Oid> = match merge_parent {
        Some(hex) => vec![Oid::from_hex(hex).ok_or_else(|| "bad mergeParent oid".to_string())?],
        None => Vec::new(),
    };
    let oid = commit_staged_with_parents(&mut repo, &identity, message, &staged, &extra).map_err(soft)?;
    Ok(json!({ "oid": oid.to_hex() }))
}

// ----- git.undo / discard / backups / restore -----

fn git_undo(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let identity = identity_of(params)?;
    let mut repo = Repo::open(StdFs::new(root)).map_err(soft)?;
    let oid = undo::undo_last_save(&mut repo, &identity).map_err(soft)?;
    Ok(json!({ "oid": oid.to_hex() }))
}

/// Discards worktree changes for the listed paths: backs the current
/// contents up first (backup commit + ref), then restores each file from
/// HEAD — or deletes it when it was never committed.
fn git_discard(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let identity = identity_of(params)?;
    let paths: Vec<String> = params
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing files".to_string())?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if paths.is_empty() {
        return Err("nothing selected to discard".into());
    }

    let mut repo = Repo::open(StdFs::new(root.clone())).map_err(soft)?;
    let head = repo.head().map_err(soft)?;
    let head_commit = head.commit.ok_or_else(|| "no saved versions yet".to_string())?;

    // Back up current contents first — the recovery path exists before
    // anything destructive happens.
    let mut backup: Vec<(String, Vec<u8>)> = Vec::new();
    for path in &paths {
        validate_rel(path)?;
        let bytes = std::fs::read(root.join(path)).unwrap_or_default();
        backup.push((path.clone(), bytes));
    }
    let backup_ref = undo::backup_before_discard(&mut repo, &identity, &backup).map_err(soft)?;

    // Restore from HEAD (or delete untracked files).
    let mut restore: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for path in &paths {
        let head_bytes = repo.read_file_at(head_commit, path).ok();
        restore.push((path.clone(), head_bytes));
    }
    undo::apply_discard(&repo, &restore).map_err(soft)?;

    Ok(json!({
        "backupRef": backup_ref.ref_name,
        "restored": restore.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
    }))
}

fn git_backups(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let mut repo = Repo::open(StdFs::new(root)).map_err(soft)?;
    let backups = undo::list_backups(&mut repo).map_err(soft)?;
    Ok(json!(backups
        .iter()
        .map(|b| json!({
            "ref": b.ref_name,
            "when": b.when,
            "paths": b.paths,
        }))
        .collect::<Vec<_>>()))
}

/// Re-applies a discard backup's contents to the worktree ("undo the
/// discard"), backing up what's there now first.
fn git_restore(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let identity = identity_of(params)?;
    let ref_name = param_str(params, "backupRef")?;
    if !ref_name.starts_with("refs/tpt-git-simple/backups/") {
        return Err("not a Git Simple backup ref".into());
    }

    let mut repo = Repo::open(StdFs::new(root.clone())).map_err(soft)?;
    let backups = undo::list_backups(&mut repo).map_err(soft)?;
    let backup = backups
        .into_iter()
        .find(|b| b.ref_name == ref_name)
        .ok_or_else(|| "that backup no longer exists".to_string())?;
    let contents = undo::restore_backup_contents(&mut repo, &backup).map_err(soft)?;

    // Guard the restore the same way: back up the CURRENT state first.
    let mut backup_now: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, _) in &contents {
        let bytes = std::fs::read(root.join(path)).unwrap_or_default();
        backup_now.push((path.clone(), bytes));
    }
    let _new_backup = undo::backup_before_discard(&mut repo, &identity, &backup_now).map_err(soft)?;
    let apply: Vec<(String, Option<Vec<u8>>)> =
        contents.into_iter().map(|(p, b)| (p, Some(b))).collect();
    undo::apply_discard(&repo, &apply).map_err(soft)?;
    Ok(json!({ "restored": apply.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>() }))
}

// ----- git.pull / git.push -----

fn git_pull(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let identity = identity_of(params)?;

    // Which remote/branch does the current one track?
    let upstream = run_git(&root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"], true)
        .map_err(|_| "This branch isn't connected to a copy online yet. Once it's been pushed (sent), getting the latest will work.".to_string())?;
    let upstream = upstream.trim().to_string();
    let (remote, _branch) = upstream
        .split_once('/')
        .ok_or_else(|| format!("can't read the branch link {upstream:?}"))?;

    // Fetch (may prompt for credentials in its own window).
    run_git(&root, &["fetch", remote], true).map_err(|e| format!("couldn't reach {remote}: {e}"))?;
    let theirs_hex = run_git(&root, &["rev-parse", "FETCH_HEAD"], true)?
        .trim()
        .to_string();
    let theirs = Oid::from_hex(&theirs_hex).ok_or_else(|| "bad FETCH_HEAD".to_string())?;

    let mut repo = Repo::open(StdFs::new(root.clone())).map_err(soft)?;
    let head = repo.head().map_err(soft)?;
    let ours = head.commit.ok_or_else(|| "no saved versions yet".to_string())?;
    if head.branch.is_none() {
        return Err("HEAD is detached — open a branch to get the latest".into());
    }

    let base = repo.merge_base(ours, theirs).map_err(soft)?;
    if Some(theirs) == base {
        return Ok(json!({ "status": "up-to-date" }));
    }

    let result = merge::merge_trees(&mut repo, base, ours, theirs).map_err(soft)?;

    // Materialize the merge outcome into worktree writes.
    let theirs_map = repo.tree_map(theirs).map_err(soft)?;
    let mut staged: Vec<StagedFile> = Vec::new();
    let mut conflicts: Vec<Value> = Vec::new();
    for (path, outcome) in &result.outcomes {
        match outcome {
            MergeOutcome::Unchanged | MergeOutcome::TookOurs => {}
            MergeOutcome::TookTheirs => {
                let bytes = theirs_map
                    .get(path)
                    .map(|(_, oid)| repo.blob(oid).map_err(soft))
                    .transpose()?
                    .unwrap_or_default();
                let present = theirs_map.contains_key(path);
                write_worktree_file(&root, path, if present { Some(&bytes) } else { None })?;
                staged.push(StagedFile {
                    path: path.clone(),
                    contents: if present { Some(bytes) } else { None },
                });
            }
            MergeOutcome::Merged(bytes) => {
                write_worktree_file(&root, path, Some(bytes))?;
                staged.push(StagedFile { path: path.clone(), contents: Some(bytes.clone()) });
            }
            MergeOutcome::Conflict(f) => {
                if f.delete_modify {
                    // File already holds our edits; the user decides.
                } else {
                    write_worktree_file(&root, path, Some(&f.marked))?;
                }
                let guidance = conflict::guidance_from(f);
                conflicts.push(json!({
                    "path": f.path,
                    "deleteModify": f.delete_modify,
                    "headline": guidance.headline,
                    "steps": guidance.steps,
                    "sections": guidance.sections.iter().map(|s| json!({
                        "where": s.where_,
                        "ours": s.ours_excerpt,
                        "theirs": s.theirs_excerpt,
                    })).collect::<Vec<_>>(),
                }));
                let bytes = std::fs::read(root.join(path)).unwrap_or_default();
                staged.push(StagedFile { path: path.clone(), contents: Some(bytes) });
            }
        }
    }

    if !conflicts.is_empty() {
        return Ok(json!({
            "status": "conflicts",
            "conflicts": conflicts,
            // Completing the merge later needs this parent recorded.
            "mergeParent": theirs_hex,
        }));
    }

    // Clean merge: one merge commit records both sides.
    let oid = commit_staged_with_parents(
        &mut repo,
        &identity,
        "Get the latest version",
        &staged,
        &[theirs],
    )
    .map_err(soft)?;
    Ok(json!({ "status": "merged", "oid": oid.to_hex() }))
}

fn git_push(params: &Value) -> Result<Value, String> {
    let root = repo_path(params)?;
    let has_upstream = run_git(
        &root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        true,
    )
    .is_ok();
    let args: Vec<&str> = if has_upstream {
        vec!["push"]
    } else {
        return Err("This branch isn't connected to a copy online yet. Set that up once with the git command line (git push -u), then sending from here will work.".into());
    };
    run_git(&root, &args, true).map_err(|e| format!("sending didn't finish: {e}"))?;
    Ok(json!({ "status": "pushed" }))
}

/// Runs the git CLI in `dir`, capturing stdout. `check` makes failure an
/// error carrying stderr (trimmed). Runs on a helper thread with a timeout
/// so a hung network or credential prompt can't freeze the app forever.
fn run_git(dir: &Path, args: &[&str], check: bool) -> Result<String, String> {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output();
        let _ = tx.send(output);
    });
    let output = match rx.recv_timeout(NETWORK_TIMEOUT) {
        Ok(output) => output,
        Err(_) => {
            // The thread stays blocked on the child; detach and report.
            drop(worker);
            return Err("git took too long (over 3 minutes) — try again once any credential window is closed".into());
        }
    };
    let _ = worker.join();
    let output = output.map_err(|e| format!("couldn't run git: {e}"))?;
    if check && !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// Builds a fixture repo and returns (root, cleanup).
    fn fixture(tag: &str) -> (PathBuf, PathGuard) {
        let root = std::env::temp_dir().join(format!("gitsimple-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), PathGuard(root))
    }

    struct PathGuard(PathBuf);
    impl Drop for PathGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn commit_all(root: &Path, msg: &str) {
        run_git(root, &["add", "-A"], true).unwrap();
        run_git(
            root,
            &["commit", "-m", msg],
            true,
        )
        .unwrap();
    }

    fn val(params: Value) -> Value {
        params
    }

    #[test]
    fn snapshot_matches_disk_and_rejects_non_repo() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("snap");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"hello").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/b.md"), b"world").unwrap();
        commit_all(&root, "first");

        let snap = git_snapshot(&val(json!({ "path": root.to_string_lossy() }))).unwrap();
        let files = snap.get("files").unwrap().as_array().unwrap();
        let paths: Vec<&str> = files
            .iter()
            .map(|f| f.as_array().unwrap()[0].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["a.md", "docs/b.md"]);
        // Contents are base64 of the original bytes.
        let a = files[0].as_array().unwrap()[1].as_str().unwrap();
        assert_eq!(b64_decode(a).unwrap(), b"hello");

        let bad = git_snapshot(&val(json!({ "path": "C:/definitely/not/a/repo" })));
        assert!(bad.is_err());
    }

    #[test]
    fn save_then_undo_round_trip() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("save");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"v1").unwrap();
        commit_all(&root, "first");
        let head_before = run_git(&root, &["rev-parse", "HEAD"], true).unwrap();

        std::fs::write(root.join("b.md"), b"new file").unwrap();
        let saved = git_save(&val(json!({
            "path": root.to_string_lossy(),
            "message": "Add b.md",
            "name": "Tester",
            "email": "t@example.com",
            "files": [{ "path": "b.md", "content": b64_encode(b"new file") }],
        })))
        .unwrap();
        let oid = saved.get("oid").unwrap().as_str().unwrap();
        assert_eq!(
            run_git(&root, &["rev-parse", "HEAD"], true).unwrap().trim(),
            oid
        );
        // fsck accepts the engine's objects.
        let fsck = run_git(&root, &["fsck", "--strict"], true).unwrap();
        let _ = fsck;

        let undone = git_undo(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
        })))
        .unwrap();
        assert_eq!(
            undone.get("oid").unwrap().as_str().unwrap().to_string(),
            head_before.trim().to_string()
        );
    }

    #[test]
    fn discard_backs_up_and_restores() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("discard");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"original").unwrap();
        commit_all(&root, "first");

        std::fs::write(root.join("a.md"), b"ruined").unwrap();
        git_discard(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
            "files": ["a.md"],
        })))
        .unwrap();
        assert_eq!(std::fs::read(root.join("a.md")).unwrap(), b"original");

        // The backup exists and can be re-applied ("undo the discard").
        let backups = git_backups(&val(json!({ "path": root.to_string_lossy() }))).unwrap();
        let list = backups.as_array().unwrap();
        assert_eq!(list.len(), 1);
        let ref_name = list[0].get("ref").unwrap().as_str().unwrap();
        git_restore(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
            "backupRef": ref_name,
        })))
        .unwrap();
        assert_eq!(std::fs::read(root.join("a.md")).unwrap(), b"ruined");
    }

    #[test]
    fn pull_clean_merge_from_upstream() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("pull");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"one\n").unwrap();
        commit_all(&root, "first");

        // Pretend "origin" is a second clone.
        let origin = root.parent().unwrap().join("gitsimple-host-pull-origin");
        let _ = std::fs::remove_dir_all(&origin);
        run_git(&root, &["clone", root.to_str().unwrap(), origin.to_str().unwrap()], true).unwrap();
        // Configure the first repo's branch to track the clone (origin).
        std::fs::write(origin.join("b.md"), b"from origin\n").unwrap();
        commit_all(&origin, "origin adds b.md");
        run_git(&root, &["remote", "add", "origin", origin.to_str().unwrap()], true).unwrap();
        run_git(&root, &["fetch", "origin"], true).unwrap();
        run_git(&root, &["branch", "--set-upstream-to=origin/main", "main"], true).unwrap();

        // Local change that doesn't clash.
        std::fs::write(root.join("c.md"), b"local\n").unwrap();
        git_save(&val(json!({
            "path": root.to_string_lossy(),
            "message": "local adds c.md",
            "name": "Tester",
            "email": "t@example.com",
            "files": [{ "path": "c.md", "content": b64_encode(b"local\n") }],
        })))
        .unwrap();

        let result = git_pull(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
        })))
        .unwrap();
        assert_eq!(result.get("status").unwrap().as_str(), Some("merged"));
        assert_eq!(std::fs::read(root.join("b.md")).unwrap(), b"from origin\n");
        // The merge commit has two parents (local save + origin tip).
        let parents = run_git(&root, &["log", "-1", "--format=%P"], true).unwrap();
        assert_eq!(parents.split_whitespace().count(), 2);
    }

    #[test]
    fn pull_conflict_writes_markers_and_reports() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("conflict");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"base line\nsecond line\n").unwrap();
        commit_all(&root, "first");

        let origin = root.parent().unwrap().join("gitsimple-host-conflict-origin");
        let _ = std::fs::remove_dir_all(&origin);
        run_git(&root, &["clone", root.to_str().unwrap(), origin.to_str().unwrap()], true).unwrap();
        std::fs::write(origin.join("a.md"), b"base line\nTHEIRS line\n").unwrap();
        commit_all(&origin, "theirs edits");
        run_git(&root, &["remote", "add", "origin", origin.to_str().unwrap()], true).unwrap();
        run_git(&root, &["fetch", "origin"], true).unwrap();
        run_git(&root, &["branch", "--set-upstream-to=origin/main", "main"], true).unwrap();

        std::fs::write(root.join("a.md"), b"base line\nOURS line\n").unwrap();
        commit_all(&root, "ours edits");

        let result = git_pull(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
        })))
        .unwrap();
        assert_eq!(result.get("status").unwrap().as_str(), Some("conflicts"));
        let conflicts = result.get("conflicts").unwrap().as_array().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].get("path").unwrap().as_str(), Some("a.md"));
        assert!(conflicts[0].get("mergeParent").is_none()); // mergeParent is top-level
        let on_disk = std::fs::read_to_string(root.join("a.md")).unwrap();
        assert!(on_disk.contains("<<<<<<<"));
        assert!(on_disk.contains("OURS line"));
        assert!(on_disk.contains("THEIRS line"));
        assert!(result.get("mergeParent").unwrap().as_str().is_some());
    }

    #[test]
    fn pull_up_to_date_is_reported() {
        if !git_ok() {
            return;
        }
        let (root, _guard) = fixture("uptodate");
        run_git(&root, &["init", "-b", "main"], true).unwrap();
        std::fs::write(root.join("a.md"), b"one\n").unwrap();
        commit_all(&root, "first");
        let origin = root.parent().unwrap().join("gitsimple-host-uptodate-origin");
        let _ = std::fs::remove_dir_all(&origin);
        run_git(&root, &["clone", root.to_str().unwrap(), origin.to_str().unwrap()], true).unwrap();
        run_git(&root, &["remote", "add", "origin", origin.to_str().unwrap()], true).unwrap();
        run_git(&root, &["fetch", "origin"], true).unwrap();
        run_git(&root, &["branch", "--set-upstream-to=origin/main", "main"], true).unwrap();

        let result = git_pull(&val(json!({
            "path": root.to_string_lossy(),
            "name": "Tester",
            "email": "t@example.com",
        })))
        .unwrap();
        assert_eq!(result.get("status").unwrap().as_str(), Some("up-to-date"));
    }
}

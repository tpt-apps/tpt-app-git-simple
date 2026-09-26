//! Integration tests against REAL repositories built with the git CLI.
//!
//! The engine's claims are only as good as their agreement with git
//! itself: these tests create fixture repos with `git`, then check that
//! the engine's reads match (refs, log, trees, packed objects, diffs,
//! status) and that its writes produce repos `git fsck` accepts and `git
//! log` agrees with.
//!
//! Requires `git` on PATH; tests skip (pass trivially) when unavailable.

use std::path::PathBuf;
use std::process::Command;

use tpt_git_engine::fs::{GitFs, MemoryFs, StdFs};
use tpt_git_engine::repo::Repo;
use tpt_git_engine::status;
use tpt_git_engine::GitError;

struct TestRepo {
    root: PathBuf,
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn run_git(repo: &TestRepo, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .env("GIT_AUTHOR_NAME", "Test Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "Test Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .output()
        .expect("git failed to run");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with two commits on `main` and a packed object store.
fn fixture_repo(name: &str, pack: bool) -> (TestRepo, Repo<StdFs>) {
    let root = std::env::temp_dir().join(format!(
        "tpt-git-engine-test-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let tr = TestRepo { root: root.clone() };
    run_git(&tr, &["init", "-b", "main"]);
    std::fs::write(tr.root.join("readme.md"), "line one\nline two\nline three\n").unwrap();
    std::fs::create_dir_all(tr.root.join("docs")).unwrap();
    std::fs::write(tr.root.join("docs/guide.md"), "# Guide\n\nWelcome.\n").unwrap();
    run_git(&tr, &["add", "-A"]);
    run_git(&tr, &["commit", "-m", "First commit"]);

    std::fs::write(tr.root.join("readme.md"), "line one\nCHANGED line\nline three\n").unwrap();
    std::fs::write(tr.root.join("docs/new.md"), "fresh file\n").unwrap();
    run_git(&tr, &["add", "-A"]);
    run_git(&tr, &["commit", "-m", "Second commit: change readme, add docs/new"]);

    if pack {
        run_git(&tr, &["repack", "-a", "-d"]);
        // After repack everything is in a packfile; drop loose objects.
        let objects = tr.root.join(".git/objects");
        for entry in std::fs::read_dir(&objects).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    (tr, Repo::open(StdFs::new(root.clone())).unwrap())
}

// ---------- reads: free tier ----------

#[test]
fn head_and_branches_match_git() {
    if !git_available() {
        return;
    }
    let (_tr, mut repo) = fixture_repo("head", false);
    let head = repo.head().unwrap();
    assert_eq!(head.branch.as_deref(), Some("main"));
    assert!(head.commit.is_some());

    let branches = repo.branches().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].0, "main");
    assert_eq!(branches[0].1, head.commit.unwrap());

    // Loose ref file must agree with git's own view.
    let git_head = run_git(&_tr, &["rev-parse", "main"]);
    assert_eq!(head.commit.unwrap().to_hex(), git_head.trim());
}

#[test]
fn log_matches_git_order_and_summaries() {
    if !git_available() {
        return;
    }
    let (_tr, mut repo) = fixture_repo("log", false);
    let log = repo.log(10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].summary, "Second commit: change readme, add docs/new");
    assert_eq!(log[1].summary, "First commit");
    assert_eq!(log[0].parents, vec![log[1].oid]);
    assert_eq!(log[0].author_name, "Test Author");

    let git_log = run_git(&_tr, &["log", "--format=%s", "--reverse"]);
    let git_summaries: Vec<&str> = git_log.lines().collect();
    assert_eq!(git_summaries[0], log[1].summary);
    assert_eq!(git_summaries[1], log[0].summary);
}

#[test]
fn packed_objects_round_trip() {
    if !git_available() {
        return;
    }
    let (_tr, mut repo) = fixture_repo("pack", true);
    let log = repo.log(10).unwrap();
    assert_eq!(log.len(), 2, "log through packed objects");

    let content = repo.read_file_at(log[1].oid, "readme.md").unwrap();
    assert_eq!(content, b"line one\nline two\nline three\n");

    // Packfile without the .idx's loose counterpart.
    let tree = repo.tree_map(log[0].oid).unwrap();
    assert!(tree.contains_key("docs/new.md"));
    let blob = repo.blob(&tree["docs/new.md"].1).unwrap();
    assert_eq!(blob, b"fresh file\n");
}

#[test]
fn mixed_packed_and_loose_objects() {
    if !git_available() {
        return;
    }
    let (tr, mut repo) = fixture_repo("mixed", true);
    // A third commit stays loose.
    std::fs::write(tr.root.join("tail.md"), "loose tail\n").unwrap();
    run_git(&tr, &["add", "-A"]);
    run_git(&tr, &["commit", "-m", "Third commit (loose)"]);

    let log = repo.log(10).unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].summary, "Third commit (loose)");
    let content = repo.read_file_at(log[0].oid, "tail.md").unwrap();
    assert_eq!(content, b"loose tail\n");
}

#[test]
fn tree_listing_and_file_reads_match_git() {
    if !git_available() {
        return;
    }
    let (_tr, mut repo) = fixture_repo("tree", false);
    let head = repo.head().unwrap().commit.unwrap();
    let head_commit = repo.commit(&head).unwrap();
    let files = repo.list_tree_recursive(&head_commit.tree).unwrap();
    let paths: Vec<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(paths, vec!["docs/guide.md", "docs/new.md", "readme.md"]);

    // `git ls-tree -r` agrees.
    let ls = run_git(&_tr, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let git_paths: Vec<&str> = ls.lines().collect();
    assert_eq!(git_paths, paths);

    let guide = repo.read_file_at(head, "docs/guide.md").unwrap();
    assert_eq!(guide, b"# Guide\n\nWelcome.\n");
    assert!(matches!(
        repo.read_file_at(head, "nope.md"),
        Err(GitError::NotFound(_))
    ));
}

#[test]
fn diff_between_commits_finds_the_edits() {
    if !git_available() {
        return;
    }
    let (_tr, mut repo) = fixture_repo("diff", false);
    let log = repo.log(10).unwrap();
    let (new, old) = (log[0].oid, log[1].oid);

    let old_bytes = repo.read_file_at(old, "readme.md").unwrap();
    let new_bytes = repo.read_file_at(new, "readme.md").unwrap();
    let d = tpt_git_engine::diff::diff_blobs(&old_bytes, &new_bytes);
    assert_eq!((d.added, d.removed), (1, 1));
    assert!(d
        .hunks
        .iter()
        .any(|h| h.lines.iter().any(|(m, l)| (*m, l.as_str()) == ('+', "CHANGED line"))));
}

#[test]
fn status_sees_edits_additions_and_deletions() {
    if !git_available() {
        return;
    }
    let (tr, mut repo) = fixture_repo("status", false);
    std::fs::write(tr.root.join("readme.md"), "totally different\n").unwrap();
    std::fs::write(tr.root.join("untracked.md"), "brand new\n").unwrap();
    std::fs::remove_file(tr.root.join("docs/new.md")).unwrap();

    let st = status::status(&mut repo).unwrap();
    let find = |p: &str| st.iter().find(|e| e.path == p).map(|e| e.kind);
    assert_eq!(find("readme.md"), Some(status::ChangeKind::Modified));
    assert_eq!(find("untracked.md"), Some(status::ChangeKind::Untracked));
    assert_eq!(find("docs/new.md"), Some(status::ChangeKind::Deleted));
    // Untouched files are invisible to status.
    assert!(find("docs/guide.md").is_none());
}

#[test]
fn merge_base_finds_the_fork_point() {
    if !git_available() {
        return;
    }
    let (tr, mut repo) = fixture_repo("base", false);
    run_git(&tr, &["checkout", "-b", "feature"]);
    std::fs::write(tr.root.join("feature.md"), "feature\n").unwrap();
    run_git(&tr, &["add", "-A"]);
    run_git(&tr, &["commit", "-m", "Feature work"]);

    let git_base = run_git(&tr, &["merge-base", "main", "feature"]);
    let main = repo.resolve_ref("refs/heads/main").unwrap().unwrap();
    let feature = repo.resolve_ref("refs/heads/feature").unwrap().unwrap();
    let base = repo.merge_base(main, feature).unwrap().unwrap();
    assert_eq!(base.to_hex(), git_base.trim());
}

// ---------- writes: pro tier ----------

#[cfg(feature = "pro")]
mod pro {
    use super::*;
    use tpt_git_engine::commit::{commit_staged, Identity, StagedFile};
    use tpt_git_engine::undo;
    use tpt_git_engine::merge;

    fn identity() -> Identity {
        Identity { name: "Git Simple User".into(), email: "user@example.com".into() }
    }

    fn assert_fsck(tr: &TestRepo) {
        // `git fsck --strict` must accept everything the engine wrote.
        let out = Command::new("git")
            .args(["fsck", "--strict"])
            .current_dir(&tr.root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        // dangling objects are fine; corruption is not.
        assert!(
            out.status.success() && !stderr.contains("error"),
            "fsck failed: {stderr}"
        );
    }

    #[test]
    fn save_commits_and_git_sees_it() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("save", false);
        std::fs::write(tr.root.join("hello.md"), "hello from the engine\n").unwrap();

        let st = status::status(&mut repo).unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].path, "hello.md");

        let bytes = std::fs::read(tr.root.join("hello.md")).unwrap();
        let new_commit = commit_staged(
            &mut repo,
            &identity(),
            "Add hello.md",
            &[StagedFile { path: "hello.md".into(), contents: Some(bytes) }],
        )
        .unwrap();

        // git agrees the branch moved and the tree contains the file.
        let git_head = run_git(&tr, &["rev-parse", "main"]);
        assert_eq!(new_commit.to_hex(), git_head.trim());
        let git_ls = run_git(&tr, &["ls-tree", "-r", "--name-only", "HEAD"]);
        assert!(git_ls.lines().any(|l| l == "hello.md"));
        let git_message = run_git(&tr, &["log", "-1", "--format=%s"]);
        assert_eq!(git_message.trim(), "Add hello.md");
        let git_author = run_git(&tr, &["log", "-1", "--format=%an <%ae>"]);
        assert_eq!(git_author.trim(), "Git Simple User <user@example.com>");
        assert_fsck(&tr);

        // The reflog has the save recorded.
        let reflog = run_git(&tr, &["reflog", "--format=%gs"]);
        assert!(reflog.contains("save: Add hello.md"), "reflog: {reflog}");
    }

    #[test]
    fn save_deletion_and_modify_together() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("save2", false);
        let old_readme = std::fs::read(tr.root.join("readme.md")).unwrap();
        let mut new_readme = old_readme.clone();
        new_readme.extend_from_slice(b"appended by user\n");
        std::fs::write(tr.root.join("readme.md"), &new_readme).unwrap();
        std::fs::remove_file(tr.root.join("docs/new.md")).unwrap();

        let commit = commit_staged(
            &mut repo,
            &identity(),
            "Update readme, remove docs/new.md",
            &[
                StagedFile { path: "readme.md".into(), contents: Some(new_readme.clone()) },
                StagedFile { path: "docs/new.md".into(), contents: None },
            ],
        )
        .unwrap();

        let got = repo.read_file_at(commit, "readme.md").unwrap();
        assert_eq!(got, new_readme);
        assert!(repo.read_file_at(commit, "docs/new.md").is_err());
        let git_ls = run_git(&tr, &["ls-tree", "-r", "--name-only", "HEAD"]);
        assert!(!git_ls.lines().any(|l| l == "docs/new.md"));
        assert_fsck(&tr);
    }

    #[test]
    fn undo_last_save_restores_parent() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("undo", false);
        let before = repo.head().unwrap().commit.unwrap();
        let bytes = b"temp\n".to_vec();
        let commit = commit_staged(
            &mut repo,
            &identity(),
            "Temporary save",
            &[StagedFile { path: "temp.md".into(), contents: Some(bytes) }],
        )
        .unwrap();

        let undone_to = undo::undo_last_save(&mut repo, &identity()).unwrap();
        assert_eq!(undone_to, before);
        let git_head = run_git(&tr, &["rev-parse", "main"]);
        assert_eq!(git_head.trim(), before.to_hex());
        // The undone commit still exists (reflog), just unreferenced.
        let reflog = run_git(&tr, &["reflog"]);
        assert!(reflog.contains("undo last save"));

        let redone = undo::redo_last_undo(&mut repo).unwrap();
        assert_eq!(redone, commit);
        let git_head = run_git(&tr, &["rev-parse", "main"]);
        assert_eq!(git_head.trim(), commit.to_hex());
        assert_fsck(&tr);
    }

    #[test]
    fn discard_backup_and_restore_round_trip() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("discard", false);
        let head = repo.head().unwrap().commit.unwrap();
        let head_content = repo.read_file_at(head, "readme.md").unwrap();

        // User mangles a file, then discards.
        let mangled = b"ruined!\n".to_vec();
        std::fs::write(tr.root.join("readme.md"), &mangled).unwrap();
        let backup = undo::backup_before_discard(
            &mut repo,
            &identity(),
            &[("readme.md".into(), mangled.clone())],
        )
        .unwrap();
        undo::apply_discard(
            &repo,
            &[("readme.md".into(), Some(head_content.clone()))],
        )
        .unwrap();
        assert_eq!(std::fs::read(tr.root.join("readme.md")).unwrap(), head_content);

        // The backup holds the mangled bytes and git sees the ref.
        let backups = undo::list_backups(&mut repo).unwrap();
        assert_eq!(backups.len(), 1);
        let contents = undo::restore_backup_contents(&mut repo, &backups[0]).unwrap();
        assert_eq!(contents, vec![("readme.md".to_string(), mangled.clone())]);
        let git_ref = run_git(&tr, &["rev-parse", &backup.ref_name]);
        assert_eq!(git_ref.trim(), backup.commit.to_hex());
        assert_fsck(&tr);

        // "Keep my ruin": restoring the backup re-applies the bytes.
        undo::apply_discard(&repo, &[("readme.md".into(), Some(mangled.clone()))]).unwrap();
        assert_eq!(std::fs::read(tr.root.join("readme.md")).unwrap(), mangled);
    }

    #[test]
    fn merge_clean_changes_agrees_with_git() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("mergeclean", false);
        run_git(&tr, &["checkout", "-b", "theirs"]);
        std::fs::write(
            tr.root.join("readme.md"),
            "line one\nCHANGED line\nline three\nTHEIRS tail\n",
        )
        .unwrap();
        run_git(&tr, &["add", "-A"]);
        run_git(&tr, &["commit", "-m", "theirs edit"]);
        run_git(&tr, &["checkout", "main"]);
        std::fs::write(
            tr.root.join("docs/guide.md"),
            "# Guide\n\nWelcome.\n\nOURS extra\n",
        )
        .unwrap();
        run_git(&tr, &["add", "-A"]);
        run_git(&tr, &["commit", "-m", "ours edit"]);

        let ours = repo.resolve_ref("refs/heads/main").unwrap().unwrap();
        let theirs = repo.resolve_ref("refs/heads/theirs").unwrap().unwrap();
        let ancestor = repo.merge_base(ours, theirs).unwrap().unwrap();
        let result = merge::merge_trees(&mut repo, Some(ancestor), ours, theirs).unwrap();
        assert!(result.clean_tree.is_some(), "expected a clean merge");

        // Each side's file went to the right side untouched.
        assert!(matches!(
            result.outcomes.get("readme.md"),
            Some(merge::MergeOutcome::TookTheirs)
        ));
        assert!(matches!(
            result.outcomes.get("docs/guide.md"),
            Some(merge::MergeOutcome::TookOurs)
        ));

        // git's own auto-merge produces the same readme content.
        run_git(&tr, &["merge", "theirs", "--no-edit"]);
        let git_readme = run_git(&tr, &["show", "HEAD:readme.md"]);
        let expected =
            repo.read_file_at(theirs, "readme.md").unwrap();
        assert_eq!(git_readme.into_bytes(), expected);
    }

    #[test]
    fn merge_conflict_produces_state_and_matches_git() {
        if !git_available() {
            return;
        }
        let (tr, mut repo) = fixture_repo("conflict", false);
        run_git(&tr, &["checkout", "-b", "theirs"]);
        std::fs::write(tr.root.join("readme.md"), "line one\nTHEIRS line\nline three\n").unwrap();
        run_git(&tr, &["add", "-A"]);
        run_git(&tr, &["commit", "-m", "theirs edit"]);
        run_git(&tr, &["checkout", "main"]);
        std::fs::write(tr.root.join("readme.md"), "line one\nOURS line\nline three\n").unwrap();
        run_git(&tr, &["add", "-A"]);
        run_git(&tr, &["commit", "-m", "ours edit"]);

        let ours = repo.resolve_ref("refs/heads/main").unwrap().unwrap();
        let theirs = repo.resolve_ref("refs/heads/theirs").unwrap().unwrap();
        let ancestor = repo.merge_base(ours, theirs).unwrap().unwrap();
        let result = merge::merge_trees(&mut repo, Some(ancestor), ours, theirs).unwrap();
        assert!(result.clean_tree.is_none());
        let conflict = result
            .outcomes
            .get("readme.md")
            .expect("readme.md in conflict state");
        let hunks = match conflict {
            merge::MergeOutcome::Conflict(f) => &f.hunks,
            other => panic!("expected conflict, got {other:?}"),
        };
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].ours, vec!["OURS line".to_string()]);
        assert_eq!(hunks[0].theirs, vec!["THEIRS line".to_string()]);

        // git sees the same clash.
        let git_merge = Command::new("git")
            .args(["merge", "theirs", "--no-edit"])
            .current_dir(&tr.root)
            .output()
            .unwrap();
        assert!(!git_merge.status.success(), "git should conflict too");
        run_git(&tr, &["merge", "--abort"]);
    }

    #[test]
    fn memory_fs_and_std_fs_read_the_same_repo() {
        if !git_available() {
            return;
        }
        // The browser path: snapshot files into a MemoryFs, read through it.
        let (tr, mut std_repo) = fixture_repo("memory", false);
        let mut mem = MemoryFs::new();
        for rel in [".git/HEAD", ".git/refs/heads/main", ".git/config"] {
            let bytes = std::fs::read(tr.root.join(&rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
                .unwrap();
            mem.add(rel, bytes);
        }
        // Objects: loose second commit + packed? (unpack -a keeps loose)
        for entry in walk(&tr.root.join(".git/objects")) {
            let rel = entry
                .strip_prefix(&tr.root)
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            mem.add(&rel, std::fs::read(&entry).unwrap());
        }
        let mut mem_repo = Repo::open(mem).unwrap();
        let a = std_repo.log(10).unwrap();
        let b = mem_repo.log(10).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].oid, b[0].oid);
        assert_eq!(a[0].summary, b[0].summary);
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
}

#[test]
fn every_packed_object_reads_and_matches_git() {
    if !git_available() {
        return;
    }
    let (tr, mut repo) = fixture_repo("probe", true);
    let pack_dir = tr.root.join(".git/objects/pack");
    let mut idx_file: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&pack_dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().map(|e| e == "idx").unwrap_or(false) {
            idx_file = Some(p.clone());
        }
    }
    let idx_file = idx_file.unwrap();
    let idx_bytes = std::fs::read(&idx_file).unwrap();
    let count =
        u32::from_be_bytes(idx_bytes[8 + 255 * 4..8 + 256 * 4].try_into().unwrap()) as usize;

    for i in 0..count {
        let at = 8 + 256 * 4 + i * 20;
        let mut raw = [0u8; 20];
        raw.copy_from_slice(&idx_bytes[at..at + 20]);
        let oid = tpt_git_engine::Oid(raw);
        let obj = repo.read_object(&oid).unwrap_or_else(|e| {
            panic!("object {} unreadable: {e}", oid.to_hex())
        });
        // Cross-check type and size against git's own view.
        let hex = oid.to_hex();
        let git_type = run_git(&tr, &["cat-file", "-t", &hex]);
        let git_size: usize = run_git(&tr, &["cat-file", "-s", &hex])
            .trim()
            .parse()
            .unwrap();
        let kind_ok = format!("{:?}", obj.kind).to_lowercase().contains(git_type.trim());
        assert!(kind_ok, "{}: engine type {:?} vs git {}", hex, obj.kind, git_type.trim());
        assert_eq!(obj.data.len(), git_size, "{}: size mismatch", hex);
    }
}

//! The read-side repo API over any [`GitFs`]: refs/HEAD resolution, object
//! access (loose + packed), commit log, recursive tree listing, and
//! file-at-commit reads.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::{GitError, Result};
use crate::fs::GitFs;
use crate::objects::{parse_commit, parse_loose, parse_tree, Commit, Object, ObjectType, TreeEntry};
use crate::oid::Oid;
use crate::pack::Pack;

/// Resolved branch state: where the checked-out branch points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    /// `main` for `refs/heads/main`; `None` for detached HEAD.
    pub branch: Option<String>,
    /// What HEAD points at. `None` on a fresh repo with no commits yet.
    pub commit: Option<Oid>,
}

pub struct Repo<F: GitFs> {
    fs: F,
    /// Opened lazily from `.git/objects/pack/`.
    packs: Vec<Pack>,
    /// Commit cache for log walks (bounded by target-repo size in practice).
    commits: HashMap<Oid, Commit>,
}

/// One entry of a `log` listing, newest first.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    pub time: i64,
    pub summary: String,
    /// Full commit message (summary + body) — the summary is the first
    /// paragraph, the body the rest.
    pub message: String,
}

impl<F: GitFs> Repo<F> {
    /// Opens a repo rooted at `fs` (i.e. `fs` resolves `.git/…` paths).
    pub fn open(fs: F) -> Result<Self> {
        if !fs.exists(".git/HEAD") {
            return Err(GitError::NotARepository(
                "no .git/HEAD found at the selected folder".into(),
            ));
        }
        Ok(Repo { fs, packs: Vec::new(), commits: HashMap::new() })
    }

    pub fn fs(&self) -> &F {
        &self.fs
    }

    // ----- refs & HEAD -----

    /// Resolves `.git/HEAD` to its branch and commit.
    pub fn head(&mut self) -> Result<HeadInfo> {
        let raw = String::from_utf8_lossy(&self.fs.read_file(".git/HEAD")?).into_owned();
        let raw = raw.trim_end();
        if let Some(ref_path) = raw.strip_prefix("ref: ") {
            let branch = ref_path
                .strip_prefix("refs/heads/")
                .ok_or_else(|| GitError::Corrupt(format!("HEAD points at {ref_path}")))?;
            let commit = self.resolve_ref(ref_path)?;
            Ok(HeadInfo { branch: Some(branch.to_string()), commit })
        } else {
            let commit = Oid::from_hex(raw)
                .ok_or_else(|| GitError::Corrupt("HEAD is neither a ref nor an oid".into()))?;
            Ok(HeadInfo { branch: None, commit: Some(commit) })
        }
    }

    /// The current branch name, if HEAD is attached.
    pub fn current_branch(&mut self) -> Result<Option<String>> {
        Ok(self.head()?.branch)
    }

    /// All branch heads: loose refs under `refs/heads/` merged over
    /// `packed-refs`. Sorted by name.
    pub fn branches(&mut self) -> Result<Vec<(String, Oid)>> {
        let mut out: Vec<(String, Oid)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (name, oid) in self.packed_refs()? {
            if let Some(branch) = name.strip_prefix("refs/heads/") {
                seen.insert(branch.to_string());
                out.push((branch.to_string(), oid));
            }
        }
        self.collect_loose_refs("refs/heads", "", &mut out, &mut seen)?;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// All remote-tracking refs (`<remote>/<branch>` under
    /// `refs/remotes/`).
    pub fn remote_branches(&mut self) -> Result<Vec<(String, Oid)>> {
        let mut out: Vec<(String, Oid)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (name, oid) in self.packed_refs()? {
            if let Some(rest) = name.strip_prefix("refs/remotes/") {
                seen.insert(rest.to_string());
                out.push((rest.to_string(), oid));
            }
        }
        self.collect_loose_refs("refs/remotes", "", &mut out, &mut seen)?;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Resolves a full ref name (`refs/heads/main`) through loose refs and
    /// `packed-refs`, following one level of symbolic indirection.
    pub fn resolve_ref(&self, ref_name: &str) -> Result<Option<Oid>> {
        let loose_path = format!(".git/{ref_name}");
        if self.fs.exists(&loose_path) {
            let raw = String::from_utf8_lossy(&self.fs.read_file(&loose_path)?).into_owned();
            let raw = raw.trim_end();
            if let Some(target) = raw.strip_prefix("ref: ") {
                return self.resolve_ref(target);
            }
            return Oid::from_hex(raw)
                .map(Some)
                .ok_or_else(|| GitError::Corrupt(format!("ref {ref_name} has a bad oid")));
        }
        Ok(self.packed_refs()?.into_iter().find(|(name, _)| name == ref_name).map(|(_, oid)| oid))
    }

    /// Parses `.git/packed-refs` (with `^`-peeled lines ignored).
    fn packed_refs(&self) -> Result<Vec<(String, Oid)>> {
        if !self.fs.exists(".git/packed-refs") {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&self.fs.read_file(".git/packed-refs")?).into_owned();
        let mut out = Vec::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            let mut parts = line.splitn(2, ' ');
            let oid = match parts.next().and_then(Oid::from_hex) {
                Some(oid) => oid,
                None => continue,
            };
            if let Some(name) = parts.next() {
                out.push((name.to_string(), oid));
            }
        }
        Ok(out)
    }

    /// Recursively gathers loose refs under `.git/<base>/…` (e.g. base
    /// `refs/heads`) into `(name-without-base, oid)` pairs. `seen` skips
    /// names already taken from packed-refs.
    fn collect_loose_refs(
        &self,
        base: &str,
        prefix: &str,
        out: &mut Vec<(String, Oid)>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let dir = if prefix.is_empty() {
            format!(".git/{base}")
        } else {
            format!(".git/{base}/{prefix}")
        };
        let names = match self.fs.list_dir(&dir) {
            Ok(names) => names,
            Err(GitError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        for name in names {
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let path = format!("{dir}/{name}");
            // Directory if list_dir succeeds; on a file Windows gives an Io
            // error ("directory name is invalid"), not NotFound.
            match self.fs.list_dir(&path) {
                Ok(_) => self.collect_loose_refs(base, &rel, out, seen)?,
                Err(GitError::NotFound(_)) | Err(GitError::Io(_)) => {
                    if let Ok(raw) = self.fs.read_file(&path) {
                        let raw = String::from_utf8_lossy(&raw).trim_end().to_string();
                        let oid = match raw.strip_prefix("ref: ") {
                            Some(target) => self.resolve_ref(target).ok().flatten(),
                            None => Oid::from_hex(&raw),
                        };
                        if let Some(oid) = oid {
                            if seen.insert(rel.clone()) {
                                out.push((rel, oid));
                            }
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    // ----- object access -----

    /// Loads packfiles on first use (and after an explicit refresh).
    fn ensure_packs(&mut self) -> Result<()> {
        if !self.packs.is_empty() {
            return Ok(());
        }
        let pack_dir = ".git/objects/pack";
        let names = match self.fs.list_dir(pack_dir) {
            Ok(names) => names,
            Err(GitError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        for name in names.iter().filter(|n| n.ends_with(".idx")) {
            let stem = name.trim_end_matches(".idx");
            let pack_name = format!("{stem}.pack");
            if !self.fs.exists(&format!("{pack_dir}/{pack_name}")) {
                continue;
            }
            let idx = self.fs.read_file(&format!("{pack_dir}/{name}"))?;
            let pack = self.fs.read_file(&format!("{pack_dir}/{pack_name}"))?;
            match Pack::open(idx, pack) {
                Ok(pack) => self.packs.push(pack),
                // A single bad packfile shouldn't take the whole repo down.
                Err(_) => continue,
            }
        }
        Ok(())
    }

    /// Reads an object by oid: loose first, then packfiles.
    pub fn read_object(&mut self, oid: &Oid) -> Result<Object> {
        self.ensure_packs()?;
        let loose = self.fs.read_file(&format!(".git/objects/{}", oid.loose_path()));
        if let Ok(bytes) = loose {
            return parse_loose(&bytes);
        }
        for pack in &self.packs {
            if pack.contains(oid) {
                let (kind, data) = pack.read_object(oid)?;
                return Ok(Object { kind, data });
            }
        }
        Err(GitError::NotFound(format!("object {}", oid.to_hex())))
    }

    /// Reads and parses a commit, memoized for log walks.
    pub fn commit(&mut self, oid: &Oid) -> Result<Commit> {
        if let Some(cached) = self.commits.get(oid) {
            return Ok(cached.clone());
        }
        let obj = self.read_object(oid)?;
        if obj.kind != ObjectType::Commit {
            return Err(GitError::Corrupt(format!("{} is not a commit", oid.to_hex())));
        }
        let commit = parse_commit(&obj.data)?;
        self.commits.insert(*oid, commit.clone());
        Ok(commit)
    }

    /// Parses a tree object by oid.
    pub fn tree(&mut self, oid: &Oid) -> Result<Vec<TreeEntry>> {
        let obj = self.read_object(oid)?;
        if obj.kind != ObjectType::Tree {
            return Err(GitError::Corrupt(format!("{} is not a tree", oid.to_hex())));
        }
        parse_tree(&obj.data)
    }

    /// Reads a blob's bytes by oid.
    pub fn blob(&mut self, oid: &Oid) -> Result<Vec<u8>> {
        let obj = self.read_object(oid)?;
        if obj.kind != ObjectType::Blob {
            return Err(GitError::Corrupt(format!("{} is not a blob", oid.to_hex())));
        }
        Ok(obj.data)
    }

    // ----- history -----

    /// Commit history reachable from HEAD (or any oid), newest first — a
    /// plain BFS by commit time, which for the target repos (docs/content)
    /// matches `git log` order.
    pub fn log(&mut self, limit: usize) -> Result<Vec<LogEntry>> {
        let head = self.head()?;
        let Some(start) = head.commit else {
            return Ok(Vec::new());
        };
        self.log_from(start, limit)
    }

    pub fn log_from(&mut self, start: Oid, limit: usize) -> Result<Vec<LogEntry>> {
        let mut out: Vec<LogEntry> = Vec::new();
        let mut queued: HashSet<Oid> = HashSet::new();
        let mut frontier: VecDeque<Oid> = VecDeque::new();
        frontier.push_back(start);
        queued.insert(start);

        while let Some(oid) = frontier.pop_front() {
            let commit = self.commit(&oid)?;
            for parent in &commit.parents {
                if queued.insert(*parent) {
                    frontier.push_back(*parent);
                }
            }
            out.push(LogEntry {
                oid,
                parents: commit.parents.clone(),
                author_name: commit.author.name.clone(),
                author_email: commit.author.email.clone(),
                time: commit.committer.time,
                summary: first_paragraph(&commit.message),
                message: commit.message.clone(),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// The best common ancestor of two commits: the shared ancestor with
    /// the fewest hops from `a` (i.e. closest to the tips), commit time as
    /// a tie-break. Bounded walk, correct for the small/content repos this
    /// app targets.
    pub fn merge_base(&mut self, a: Oid, b: Oid) -> Result<Option<Oid>> {
        const WALK_CAP: usize = 10_000;
        let (set_a, _) = self.ancestor_set(a, WALK_CAP)?;
        let (set_b, _) = self.ancestor_set(b, WALK_CAP)?;
        let shared: HashSet<Oid> = set_a.intersection(&set_b).copied().collect();
        let Some(first) = shared.iter().next().copied() else {
            return Ok(None);
        };
        let depths = self.depths_from(a, WALK_CAP)?;
        let mut best = first;
        let mut best_depth = depths.get(&best).copied().unwrap_or(usize::MAX);
        let mut best_time = self.commit(&best)?.committer.time;
        for oid in shared {
            let d = depths.get(&oid).copied().unwrap_or(usize::MAX);
            let t = self.commit(&oid)?.committer.time;
            if d < best_depth || (d == best_depth && t > best_time) {
                best = oid;
                best_depth = d;
                best_time = t;
            }
        }
        Ok(Some(best))
    }

    /// BFS hop-counts from `start` to every reachable ancestor.
    fn depths_from(&mut self, start: Oid, cap: usize) -> Result<HashMap<Oid, usize>> {
        let mut depths = HashMap::new();
        let mut queue = VecDeque::new();
        queue.push_back((start, 0usize));
        while let Some((oid, d)) = queue.pop_front() {
            if depths.contains_key(&oid) {
                continue;
            }
            depths.insert(oid, d);
            if depths.len() >= cap {
                break;
            }
            for parent in self.commit(&oid)?.parents {
                queue.push_back((parent, d + 1));
            }
        }
        Ok(depths)
    }

    fn ancestor_set(&mut self, start: Oid, cap: usize) -> Result<(HashSet<Oid>, usize)> {
        let mut set = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        while let Some(oid) = queue.pop_front() {
            if !set.insert(oid) {
                continue;
            }
            if set.len() >= cap {
                break;
            }
            for parent in self.commit(&oid)?.parents {
                queue.push_back(parent);
            }
        }
        let len = set.len();
        Ok((set, len))
    }

    // ----- trees & files -----

    /// Recursively lists every file path in a tree: `(path, mode, oid)`.
    pub fn list_tree_recursive(&mut self, tree_oid: &Oid) -> Result<Vec<(String, String, Oid)>> {
        let mut out = Vec::new();
        self.walk_tree(tree_oid, "", &mut out)?;
        Ok(out)
    }

    fn walk_tree(
        &mut self,
        tree_oid: &Oid,
        prefix: &str,
        out: &mut Vec<(String, String, Oid)>,
    ) -> Result<()> {
        for entry in self.tree(tree_oid)? {
            let path = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            if entry.is_dir() {
                self.walk_tree(&entry.oid, &path, out)?;
            } else {
                out.push((path, entry.mode.clone(), entry.oid));
            }
        }
        Ok(())
    }

    /// Reads one file's content at a commit. Returns `NotFound` if the path
    /// doesn't exist in that tree.
    pub fn read_file_at(&mut self, commit: Oid, path: &str) -> Result<Vec<u8>> {
        let commit = self.commit(&commit)?;
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = commit.tree;
        for (i, segment) in segments.iter().enumerate() {
            let entries = self.tree(&current)?;
            let entry = entries
                .into_iter()
                .find(|e| e.name == *segment)
                .ok_or_else(|| GitError::NotFound(path.to_string()))?;
            if i == segments.len() - 1 {
                if entry.is_dir() {
                    return Err(GitError::NotFound(path.to_string()));
                }
                return self.blob(&entry.oid);
            }
            if !entry.is_dir() {
                return Err(GitError::NotFound(path.to_string()));
            }
            current = entry.oid;
        }
        Err(GitError::NotFound(path.to_string()))
    }

    /// Map of every path in a commit's tree to `(mode, blob oid)` — the base
    /// for status comparisons and tree merges.
    pub fn tree_map(&mut self, commit: Oid) -> Result<HashMap<String, (String, Oid)>> {
        let commit = self.commit(&commit)?;
        let files = self.list_tree_recursive(&commit.tree)?;
        Ok(files.into_iter().map(|(path, mode, oid)| (path, (mode, oid))).collect())
    }
}

fn first_paragraph(message: &str) -> String {
    let message = message.trim_start();
    match message.find("\n\n") {
        Some(i) => message[..i].to_string(),
        None => message.trim_end().to_string(),
    }
}

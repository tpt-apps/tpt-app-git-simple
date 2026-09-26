//! 3-way merge (pro): get-latest combines the user's branch with the
//! fetched remote branch. Blob-level, diff3-style line merging; conflicts
//! become conflict markers plus a machine-readable state the UI turns into
//! plain-English guidance ([`crate::conflict`]).

use std::collections::BTreeMap;

use crate::diff::changed_regions;
use crate::error::{GitError, Result};
use crate::fs::GitFs;
use crate::oid::Oid;
use crate::repo::Repo;

/// What happened to one path in a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Only one side touched it — that side won.
    TookOurs,
    TookTheirs,
    /// Identical on the relevant sides — nothing to do.
    Unchanged,
    /// Clean line-level 3-way merge; content is the merged bytes.
    Merged(Vec<u8>),
    /// Both sides changed the same regions; the state feeds the conflict
    /// wizard.
    Conflict(ConflictFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    pub path: String,
    /// Set when one side deleted the file while the other edited it — the
    /// hunks then carry the surviving side only.
    pub delete_modify: bool,
    pub hunks: Vec<ConflictHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConflictHunk {
    /// Lines from the user's branch.
    pub ours: Vec<String>,
    /// Lines from the incoming (remote) branch.
    pub theirs: Vec<String>,
    /// Lines both started from, when available.
    pub base: Option<Vec<String>>,
}

pub struct MergeResult {
    /// Per-path outcomes for the union of the three trees.
    pub outcomes: BTreeMap<String, MergeOutcome>,
    /// New tree oid when the whole merge was clean (`None` on conflict).
    pub clean_tree: Option<Oid>,
}

const MARKER_OURS: &str = "<<<<<<<";
const MARKER_SEP: &str = "=======";
const MARKER_THEIRS: &str = ">>>>>>>";

/// Merges `theirs` (incoming) into `ours` using `base` (the common
/// ancestor). Doesn't touch the worktree or refs — returns what each path
/// should become; the caller applies it and the UI narrates.
pub fn merge_trees<F: GitFs>(
    repo: &mut Repo<F>,
    base: Option<Oid>,
    ours: Oid,
    theirs: Oid,
) -> Result<MergeResult> {
    let base_map = match base {
        Some(commit) => repo.tree_map(commit)?,
        None => std::collections::HashMap::new(),
    };
    let ours_map = repo.tree_map(ours)?;
    let theirs_map = repo.tree_map(theirs)?;

    let mut paths: Vec<&String> = ours_map.keys().chain(theirs_map.keys()).collect();
    paths.extend(base_map.keys());
    paths.sort();
    paths.dedup();

    let mut outcomes = BTreeMap::new();
    let mut conflicted = false;

    for path in paths {
        let b = base_map.get(path).map(|(_, oid)| *oid);
        let o = ours_map.get(path).map(|(_, oid)| *oid);
        let t = theirs_map.get(path).map(|(_, oid)| *oid);

        let outcome = match (b, o, t) {
            (_, Some(o), Some(t)) if o == t => MergeOutcome::Unchanged,
            (_, Some(o), None) if Some(o) == b => MergeOutcome::TookTheirs, // they deleted
            (_, None, Some(t)) if Some(t) == b => MergeOutcome::TookOurs,   // we deleted
            (_, None, None) => MergeOutcome::Unchanged,                     // both deleted
            (_, Some(_), None) if b.is_none() => MergeOutcome::TookOurs,   // only we added
            (_, None, Some(_)) if b.is_none() => MergeOutcome::TookTheirs, // only they added
            (_, Some(_), Some(t)) if Some(t) == b => MergeOutcome::TookOurs, // only we changed it
            (_, Some(o), Some(_)) if Some(o) == b => MergeOutcome::TookTheirs, // only they changed it
            (Some(_), Some(_), None) => {
                // We edited, they deleted the file.
                conflicted = true;
                MergeOutcome::Conflict(ConflictFile {
                    path: path.clone(),
                    delete_modify: true,
                    hunks: vec![ConflictHunk {
                        ours: file_lines(repo, o)?,
                        theirs: Vec::new(),
                        base: None,
                    }],
                })
            }
            (Some(_), None, Some(_)) => {
                // They edited, we deleted the file.
                conflicted = true;
                MergeOutcome::Conflict(ConflictFile {
                    path: path.clone(),
                    delete_modify: true,
                    hunks: vec![ConflictHunk {
                        ours: Vec::new(),
                        theirs: file_lines(repo, t)?,
                        base: None,
                    }],
                })
            }
            _ => {
                // Both sides have content and at least one changed it.
                let ours_bytes = file_bytes(repo, o)?;
                let theirs_bytes = file_bytes(repo, t)?;
                let base_bytes = file_bytes(repo, b)?;
                match merge_blobs(&base_bytes, &ours_bytes, &theirs_bytes, path)? {
                    Either::Merged(bytes) => {
                        if bytes == ours_bytes {
                            MergeOutcome::Unchanged
                        } else if bytes == theirs_bytes {
                            MergeOutcome::TookTheirs
                        } else {
                            MergeOutcome::Merged(bytes)
                        }
                    }
                    Either::Conflict(hunks) => {
                        conflicted = true;
                        MergeOutcome::Conflict(ConflictFile {
                            path: path.clone(),
                            delete_modify: false,
                            hunks,
                        })
                    }
                }
            }
        };
        outcomes.insert(path.clone(), outcome);
    }

    let clean_tree = if conflicted {
        None
    } else {
        let mut files: Vec<(String, Oid)> = Vec::new();
        for (path, outcome) in &outcomes {
            let oid = match outcome {
                MergeOutcome::TookOurs | MergeOutcome::Unchanged => {
                    ours_map.get(path).map(|(_, oid)| *oid)
                }
                MergeOutcome::TookTheirs => theirs_map.get(path).map(|(_, oid)| *oid),
                MergeOutcome::Merged(bytes) => Some(crate::commit::write_object(
                    repo,
                    crate::objects::ObjectType::Blob,
                    bytes,
                )?),
                MergeOutcome::Conflict(_) => None,
            };
            if let Some(oid) = oid {
                files.push((path.clone(), oid));
            }
        }
        Some(crate::commit::write_tree_from_paths(repo, &files)?)
    };

    Ok(MergeResult { outcomes, clean_tree })
}

fn file_lines<F: GitFs>(repo: &mut Repo<F>, oid: Option<Oid>) -> Result<Vec<String>> {
    Ok(lines(&file_bytes(repo, oid)?))
}

fn file_bytes<F: GitFs>(repo: &mut Repo<F>, oid: Option<Oid>) -> Result<Vec<u8>> {
    match oid {
        Some(oid) => repo.blob(&oid),
        None => Ok(Vec::new()),
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut l: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    if l.last().map(|s| s.is_empty()).unwrap_or(false) {
        l.pop();
    }
    l
}

pub enum Either {
    Merged(Vec<u8>),
    Conflict(Vec<ConflictHunk>),
}

/// diff3-style merge: each side's changes against the base become regions;
/// non-overlapping regions apply cleanly, overlapping ones become conflict
/// hunks with markers.
pub fn merge_blobs(base: &[u8], ours: &[u8], theirs: &[u8], path: &str) -> Result<Either> {
    if base.contains(&0) || ours.contains(&0) || theirs.contains(&0) {
        return Err(GitError::Corrupt(format!("binary file can't be merged: {path}")));
    }
    let base_l = lines(base);
    let ours_l = lines(ours);
    let theirs_l = lines(theirs);
    let base_refs: Vec<&str> = base_l.iter().map(|s| s.as_str()).collect();
    let ours_refs: Vec<&str> = ours_l.iter().map(|s| s.as_str()).collect();
    let theirs_refs: Vec<&str> = theirs_l.iter().map(|s| s.as_str()).collect();
    let ours_regions = changed_regions(&base_refs, &ours_refs);
    let theirs_regions = changed_regions(&base_refs, &theirs_refs);

    // One timeline of edits sorted by base position.
    let mut edits: Vec<(usize, usize, Vec<String>, bool)> = Vec::new();
    for (start, len, repl) in ours_regions {
        edits.push((start, len, repl, true));
    }
    for (start, len, repl) in theirs_regions {
        edits.push((start, len, repl, false));
    }
    edits.sort_by_key(|(start, _, _, _)| *start);

    let mut out: Vec<String> = Vec::new();
    let mut hunks: Vec<ConflictHunk> = Vec::new();
    let mut copied = 0usize; // base lines already copied to `out`
    let mut idx = 0usize;

    while idx < edits.len() {
        let (start, len, _, _) = &edits[idx];
        let (start, len) = (*start, *len);
        // Group every edit touching [start, start+len) — adjacency counts
        // as touching (git merges adjacent chunks), and same-point
        // insertions (len 0) group with each other. Identical replacements
        // from both sides are one clean change, not a conflict.
        let mut group_end = start + len;
        let mut group: Vec<(usize, usize, Vec<String>, bool)> = vec![edits[idx].clone()];
        let mut j = idx + 1;
        while j < edits.len() && edits[j].0 <= group_end {
            let (s2, l2, r2, side2) = &edits[j];
            let duplicate_of_other_side = group
                .iter()
                .any(|(_, _, r, s)| s != side2 && r == r2);
            if duplicate_of_other_side {
                j += 1; // same change made by both sides
                continue;
            }
            group_end = group_end.max(s2 + l2);
            group.push(edits[j].clone());
            j += 1;
        }

        // Untouched base lines before this region.
        while copied < start {
            out.push(base_l[copied].clone());
            copied += 1;
        }

        let has_ours = group.iter().any(|(_, _, _, o)| *o);
        let has_theirs = group.iter().any(|(_, _, _, o)| !*o);
        let mut distinct: Vec<&Vec<String>> = Vec::new();
        for (_, _, r, _) in &group {
            if !distinct.iter().any(|existing| *existing == r) {
                distinct.push(r);
            }
        }
        if distinct.len() == 1 {
            // Only one distinct change here (one side, or both identical).
            out.extend(distinct[0].iter().cloned());
        } else if has_ours && has_theirs {
            // Both sides changed the same region differently.
            let ours_lines: Vec<String> = group
                .iter()
                .filter(|(_, _, _, o)| *o)
                .flat_map(|(_, _, r, _)| r.iter().cloned())
                .collect();
            let theirs_lines: Vec<String> = group
                .iter()
                .filter(|(_, _, _, o)| !*o)
                .flat_map(|(_, _, r, _)| r.iter().cloned())
                .collect();
            let base_lines: Vec<String> =
                base_l[start.min(base_l.len())..group_end.min(base_l.len())].to_vec();
            out.push(format!("{MARKER_OURS} your version of {}", display_path(path)));
            out.extend(ours_lines.iter().cloned());
            out.push(MARKER_SEP.to_string());
            out.extend(theirs_lines.iter().cloned());
            out.push(format!("{MARKER_THEIRS} the latest version of {}", display_path(path)));
            hunks.push(ConflictHunk {
                ours: ours_lines,
                theirs: theirs_lines,
                base: Some(base_lines),
            });
        } else {
            // Several edits, all from the same side — apply in order.
            for (_, _, r, _) in &group {
                out.extend(r.iter().cloned());
            }
        }
        copied = group_end;
        idx = j;
    }
    while copied < base_l.len() {
        out.push(base_l[copied].clone());
        copied += 1;
    }

    // Preserve the trailing newline when any input had one.
    let mut joined = out.join("\n").into_bytes();
    let had_trailing = base.ends_with(b"\n") || ours.ends_with(b"\n") || theirs.ends_with(b"\n");
    if had_trailing && !joined.ends_with(b"\n") {
        joined.push(b'\n');
    }

    if hunks.is_empty() {
        Ok(Either::Merged(joined))
    } else {
        Ok(Either::Conflict(hunks))
    }
}

fn display_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((_, name)) => name.to_string(),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_conflict_when_sides_edit_different_lines() {
        let base = b"a\nb\nc\nd\ne\nf\n";
        let ours = b"a\nB\nc\nd\ne\nf\n";
        let theirs = b"a\nb\nc\nd\nE\nf\n";
        match merge_blobs(base, ours, theirs, "x.txt").unwrap() {
            Either::Merged(bytes) => assert_eq!(bytes, b"a\nB\nc\nd\nE\nf\n".to_vec()),
            Either::Conflict(_) => panic!("expected a clean merge"),
        }
    }

    #[test]
    fn same_change_from_both_sides_is_clean() {
        let base = b"a\nb\nc\n";
        let ours = b"a\nX\nc\n";
        let theirs = b"a\nX\nc\n";
        match merge_blobs(base, ours, theirs, "x.txt").unwrap() {
            Either::Merged(bytes) => assert_eq!(bytes, b"a\nX\nc\n".to_vec()),
            Either::Conflict(_) => panic!("expected a clean merge"),
        }
    }

    #[test]
    fn overlapping_edits_conflict_with_markers() {
        let base = b"a\nb\nc\n";
        let ours = b"a\nMY b\nc\n";
        let theirs = b"a\nTHEIR b\nc\n";
        match merge_blobs(base, ours, theirs, "x.txt").unwrap() {
            Either::Conflict(hunks) => {
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].ours, vec!["MY b".to_string()]);
                assert_eq!(hunks[0].theirs, vec!["THEIR b".to_string()]);
                assert_eq!(hunks[0].base.as_deref(), Some(&["b".to_string()][..]));
            }
            Either::Merged(_) => panic!("expected a conflict"),
        }
    }

    #[test]
    fn additions_at_same_spot_conflict() {
        let base = b"a\n";
        let ours = b"a\nmine\n";
        let theirs = b"a\ntheirs\n";
        assert!(matches!(
            merge_blobs(base, ours, theirs, "x.txt").unwrap(),
            Either::Conflict(_)
        ));
    }
}

//! Plain-English conflict guidance (pro).
//!
//! The engine's differentiator: merges produce machine-readable
//! [`ConflictFile`] states ([`crate::merge`]) and worktree files carry
//! markers after apply; this module turns both into language a
//! non-developer can act on — what clashed, which side said what, and the
//! exact next steps to finish the save.

use crate::merge::{ConflictFile, ConflictHunk};

/// A narrated conflict, ready to render.
#[derive(Debug, Clone)]
pub struct Guidance {
    pub path: String,
    /// One-sentence plain-English description of the clash.
    pub headline: String,
    /// Per-hunk narration ("You wrote X — the latest version says Y").
    pub sections: Vec<ConflictSection>,
    /// The steps to resolve, phrased for the app's UI actions.
    pub steps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ConflictSection {
    /// e.g. "Around line 12".
    pub where_: String,
    pub ours_excerpt: Vec<String>,
    pub theirs_excerpt: Vec<String>,
}

/// Builds guidance from a merge-produced conflict state.
pub fn guidance_from(conflict: &ConflictFile) -> Guidance {
    let (what, steps) = if conflict.delete_modify {
        (
            "One of you deleted this file while the other changed it.".to_string(),
            vec![
                "Choose: keep this file (with your changes), or delete it along with the incoming changes.".to_string(),
                "Press Save when you've decided — the file's current state is what gets saved.".to_string(),
            ],
        )
    } else {
        (
            format!(
                "You and the latest version both changed {} of this file.",
                plural(conflict.hunks.len(), "section")
            ),
            vec![
                "For each section below, pick what it should say — your version, the latest version, or a mix.".to_string(),
                "Edit the file until no conflict markers remain.".to_string(),
                "Press Save to finish the merge.".to_string(),
            ],
        )
    };
    Guidance {
        path: conflict.path.clone(),
        headline: what,
        sections: conflict
            .hunks
            .iter()
            .enumerate()
            .map(|(i, hunk)| section_for(i, hunk))
            .collect(),
        steps,
    }
}

fn section_for(i: usize, hunk: &ConflictHunk) -> ConflictSection {
    ConflictSection {
        where_: format!("Section {}", ordinal(i + 1)),
        ours_excerpt: hunk.ours.iter().take(8).cloned().collect(),
        theirs_excerpt: hunk.theirs.iter().take(8).cloned().collect(),
    }
}

/// Parses markers out of a worktree file the user may have hand-edited,
/// so the wizard can re-narrate whatever state the file is in now.
/// Recognizes `<<<<<<< <label>` / optional `|||||||` base / `=======` /
/// `>>>>>>> <label>`.
pub fn parse_markers(path: &str, bytes: &[u8]) -> Option<ConflictFile> {
    if bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.split('\n');
    let mut hunks = Vec::new();
    let mut ours: Vec<String> = Vec::new();
    let mut theirs: Vec<String> = Vec::new();
    let mut base: Vec<String> = Vec::new();
    let mut state = MarkerState::None;

    while let Some(line) = lines.next() {
        match state {
            MarkerState::None => {
                if line.starts_with("<<<<<<<") {
                    state = MarkerState::Ours;
                } else {
                    // Outside markers — not part of any hunk.
                }
            }
            MarkerState::Ours => {
                if line.starts_with("=======") {
                    state = MarkerState::Theirs;
                } else if line.starts_with("|||||||") {
                    state = MarkerState::Base;
                } else {
                    ours.push(line.to_string());
                }
            }
            MarkerState::Base => {
                if line.starts_with("=======") {
                    state = MarkerState::Theirs;
                } else {
                    base.push(line.to_string());
                }
            }
            MarkerState::Theirs => {
                if line.starts_with(">>>>>>>") {
                    if !ours.is_empty() || !theirs.is_empty() {
                        hunks.push(ConflictHunk {
                            ours: std::mem::take(&mut ours),
                            theirs: std::mem::take(&mut theirs),
                            base: if base.is_empty() {
                                None
                            } else {
                                Some(std::mem::take(&mut base))
                            },
                        });
                    }
                    state = MarkerState::None;
                } else {
                    theirs.push(line.to_string());
                }
            }
        }
    }
    if hunks.is_empty() {
        None
    } else {
        Some(ConflictFile {
            path: path.to_string(),
            delete_modify: false,
            hunks,
            marked: Vec::new(),
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MarkerState {
    None,
    Ours,
    Base,
    Theirs,
}

/// Whether the worktree file still has unresolved markers.
pub fn is_resolved(bytes: &[u8]) -> bool {
    // A marker at line start is decisive; the wizard checks this again
    // before saving.
    let text = String::from_utf8_lossy(bytes);
    !text.lines().any(|l| {
        l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>") || (l == "=======")
    })
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn ordinal(n: usize) -> String {
    match n % 10 {
        1 if n % 100 != 11 => format!("{n}st"),
        2 if n % 100 != 12 => format!("{n}nd"),
        3 if n % 100 != 13 => format!("{n}rd"),
        _ => format!("{n}th"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrates_simple_conflict() {
        let conflict = ConflictFile {
            path: "docs/price.md".into(),
            delete_modify: false,
            hunks: vec![ConflictHunk {
                ours: vec!["price: 20".into()],
                theirs: vec!["price: 25".into()],
                base: Some(vec!["price: 18".into()]),
            }],
            marked: Vec::new(),
        };
        let g = guidance_from(&conflict);
        assert_eq!(g.sections.len(), 1);
        assert!(g.headline.contains("both changed"));
        assert_eq!(g.sections[0].ours_excerpt, vec!["price: 20".to_string()]);
        assert!(g.steps.iter().any(|s| s.contains("Save")));
    }

    #[test]
    fn narrates_delete_modify() {
        let conflict = ConflictFile {
            path: "old.md".into(),
            delete_modify: true,
            hunks: vec![ConflictHunk::default()],
            marked: Vec::new(),
        };
        let g = guidance_from(&conflict);
        assert!(g.headline.contains("deleted"));
    }

    #[test]
    fn parses_worktree_markers() {
        let bytes = b"top\n<<<<<<< your version\nmine\n=======\ntheirs\n>>>>>>> latest\nbottom\n";
        let parsed = parse_markers("f.txt", bytes).unwrap();
        assert_eq!(parsed.hunks.len(), 1);
        assert_eq!(parsed.hunks[0].ours, vec!["mine".to_string()]);
        assert_eq!(parsed.hunks[0].theirs, vec!["theirs".to_string()]);
        assert!(parsed.hunks[0].base.is_none());
    }

    #[test]
    fn clean_file_has_no_markers() {
        assert!(is_resolved(b"normal\ntext\n"));
        assert!(!is_resolved(b"<<<<<<< here\n"));
    }
}

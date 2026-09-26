//! Line diffs: a Myers LCS core with common prefix/suffix trimming and a
//! work budget (very-large changes collapse to one replace hunk instead of
//! exploding). Output is a small hunk model the UI renders in its own
//! styles — no ANSI/patch-text coupling.

use crate::oid::Oid;

/// A change to one file between two trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Added { new_oid: Oid },
    Deleted { old_oid: Oid },
    Modified { old_oid: Oid, new_oid: Oid },
    /// Same content, different mode (e.g. executable bit flipped).
    ModeChanged { old_mode: String, new_mode: String },
}

/// One `@@ -a +b @@ hunk`: `lines` mixes context/add/del markers with the
/// line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based first line numbers on each side.
    pub old_start: usize,
    pub new_start: usize,
    /// `(marker, line)` with marker `-`/`+`/` `.
    pub lines: Vec<(char, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// True when either side had NUL bytes; the UI shows a placeholder
    /// rather than line noise.
    pub binary: bool,
}

const CONTEXT_LINES: usize = 3;
/// Myers work budget (rows × edits); beyond this the middle region
/// collapses to one replace hunk. A content repo's giant rewrite doesn't
/// need a 40-second diff.
pub const MYERS_BUDGET: usize = 4_000_000;

/// Where `side` moved away from `base`, as `(base_start, base_len,
/// replacement_lines)` regions. The 3-way merge walks these from both
/// sides; overlapping regions from opposite sides are conflicts.
pub fn changed_regions(base: &[&str], side: &[&str]) -> Vec<(usize, usize, Vec<String>)> {
    let mut prefix = 0;
    while prefix < base.len() && prefix < side.len() && base[prefix] == side[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < base.len() - prefix
        && suffix < side.len() - prefix
        && base[base.len() - 1 - suffix] == side[side.len() - 1 - suffix]
    {
        suffix += 1;
    }
    if prefix + suffix >= base.len() && prefix + suffix >= side.len() {
        return Vec::new();
    }

    let mid_base = &base[prefix..base.len() - suffix];
    let mid_side = &side[prefix..side.len() - suffix];
    let ops = myers_ops(mid_base, mid_side);

    let mut regions: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut bi = prefix; // base line index of the walk
    let mut open_start: Option<usize> = None;
    let mut open_lines: Vec<String> = Vec::new();
    for (marker, line) in ops {
        match marker {
            ' ' => {
                if let Some(start) = open_start.take() {
                    regions.push((start, bi - start, std::mem::take(&mut open_lines)));
                }
                bi += 1;
            }
            '-' => {
                if open_start.is_none() {
                    open_start = Some(bi);
                }
                bi += 1;
            }
            '+' => {
                if open_start.is_none() {
                    open_start = Some(bi);
                }
                open_lines.push(line.to_string());
            }
            _ => unreachable!(),
        }
    }
    if let Some(start) = open_start {
        regions.push((start, bi - start, open_lines));
    }
    regions
}

/// Diffs two blobs (raw bytes).
pub fn diff_blobs(old: &[u8], new: &[u8]) -> FileDiff {
    if old.contains(&0) || new.contains(&0) {
        return FileDiff { hunks: Vec::new(), added: 0, removed: 0, binary: true };
    }
    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);
    let old_lines = split_lines(&old_text);
    let new_lines = split_lines(&new_text);
    build_diff(&old_lines, &new_lines)
}

/// Change stats without hunk construction (cheap summary line).
pub fn count_changes(old: &[u8], new: &[u8]) -> (usize, usize, bool) {
    let d = diff_blobs(old, new);
    (d.added, d.removed, d.binary)
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    // A trailing newline produces a final empty "line" that isn't one.
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// The whole pipeline for two line lists: trim, Myers the middle, group
/// into context hunks.
fn build_diff(old: &[&str], new: &[&str]) -> FileDiff {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let marked: Vec<(char, &str)> = {
        let mut m = Vec::new();
        for line in &old[..prefix] {
            m.push((' ', *line));
        }
        let mid_old = &old[prefix..old.len() - suffix];
        let mid_new = &new[prefix..new.len() - suffix];
        m.extend(myers_ops(mid_old, mid_new));
        for line in &old[old.len() - suffix..] {
            m.push((' ', *line));
        }
        m
    };

    // Line numbers per marked entry.
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut numbers: Vec<(usize, usize)> = Vec::with_capacity(marked.len());
    let (mut oi, mut ni) = (0usize, 0usize);
    for (marker, _) in &marked {
        numbers.push((oi, ni));
        match marker {
            ' ' => {
                oi += 1;
                ni += 1;
            }
            '-' => {
                removed += 1;
                oi += 1;
            }
            '+' => {
                added += 1;
                ni += 1;
            }
            _ => unreachable!(),
        }
    }

    // Selection: every +/- line plus up to CONTEXT context lines around
    // each change; runs whose gaps are under 2*CONTEXT merge into one hunk.
    let n = marked.len();
    let mut show = vec![false; n];
    for i in 0..n {
        if marked[i].0 != ' ' {
            let from = i.saturating_sub(CONTEXT_LINES);
            let to = (i + CONTEXT_LINES + 1).min(n);
            for s in &mut show[from..to] {
                *s = true;
            }
        }
    }

    let mut hunks = Vec::new();
    let mut i = 0;
    while i < n {
        if !show[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < n {
            if show[end] {
                end += 1;
                continue;
            }
            let mut next = end;
            while next < n && !show[next] {
                next += 1;
            }
            if next < n && next - end < 2 * CONTEXT_LINES {
                end = next; // gap too small: merge runs
            } else {
                break;
            }
        }
        // Hunk header numbers, git-style: the first line's 1-based numbers;
        // a hunk opening with pure inserts reports the old-side position as
        // the 0-based count of old lines consumed (0 for a new file), and
        // likewise for pure deletes on the new side.
        let (first_old, first_new) = numbers[start];
        let old_start = if marked[start].0 == '+' { numbers[start].0 } else { first_old + 1 };
        let new_start = if marked[start].0 == '-' { numbers[start].1 } else { first_new + 1 };
        let lines = (start..end)
            .map(|k| (marked[k].0, marked[k].1.to_string()))
            .collect();
        hunks.push(Hunk { old_start, new_start, lines });
        i = end;
    }

    FileDiff { hunks, added, removed, binary: false }
}

/// Myers O(ND) producing the middle region as flat ops.
fn myers_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    if old.is_empty() {
        return new.iter().map(|l| ('+', *l)).collect();
    }
    if new.is_empty() {
        return old.iter().map(|l| ('-', *l)).collect();
    }
    if old.len().saturating_mul(new.len()) > MYERS_BUDGET {
        let mut ops: Vec<(char, &str)> = old.iter().map(|l| ('-', *l)).collect();
        ops.extend(new.iter().map(|l| ('+', *l)));
        return ops;
    }

    let max = old.len() + new.len();
    // Diagonals run k ∈ [-d, d]; offset = max centers them into v[0..2*max].
    let offset = max as isize;
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::with_capacity(max);
    let mut found_d = None;
    'search: for d in 0..=max {
        trace.push(v.clone());
        let di = d as isize;
        let mut k = -di;
        while k <= di {
            let idx = (k + offset) as usize;
            let mut x = if k == -di || (k != di && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while (x as usize) < old.len()
                && (y as usize) < new.len()
                && old[x as usize] == new[y as usize]
            {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x as usize >= old.len() && y as usize >= new.len() {
                found_d = Some(d);
                break 'search;
            }
            k += 2;
        }
    }

    let Some(d_end) = found_d else {
        // Unreachable for valid inputs (d = old+new always matches).
        return old.iter().map(|l| ('-', *l)).chain(new.iter().map(|l| ('+', *l))).collect();
    };

    // Backtrack, then emit ops front-to-back.
    let mut steps: Vec<(char, usize)> = Vec::new(); // (op, 0-based source index)
    let (mut x, mut y) = (old.len() as isize, new.len() as isize);
    for d in (0..=d_end).rev() {
        let v = &trace[d];
        let di = d as isize;
        let k = x - y;
        let idx = (k + offset) as usize;
        let prev_k = if k == -di || (k != di && v[idx - 1] < v[idx + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            steps.push((' ', x as usize - 1));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                steps.push(('+', y as usize - 1));
                y -= 1;
            } else {
                steps.push(('-', x as usize - 1));
                x -= 1;
            }
        }
    }
    steps.reverse();

    let mut out = Vec::with_capacity(steps.len());
    let mut last_old = 0usize;
    let mut last_new = 0usize;
    for (marker, index) in steps {
        match marker {
            ' ' => {
                out.push((' ', old[index]));
                last_old = index + 1;
                last_new += 1;
            }
            '-' => {
                while last_old < index {
                    out.push((' ', old[last_old]));
                    last_old += 1;
                    last_new += 1;
                }
                out.push(('-', old[index]));
                last_old = index + 1;
            }
            '+' => {
                while last_new < index {
                    out.push((' ', old[last_old]));
                    last_old += 1;
                    last_new += 1;
                }
                out.push(('+', new[index]));
                last_new = index + 1;
            }
            _ => unreachable!(),
        }
    }
    while last_old < old.len() {
        out.push((' ', old[last_old]));
        last_old += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_input_has_no_hunks() {
        let d = diff_blobs(b"a\nb\nc\n", b"a\nb\nc\n");
        assert_eq!((d.added, d.removed), (0, 0));
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn simple_edit_one_hunk() {
        let d = diff_blobs(b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n", b"a\nb\nc\nD\ne\nf\ng\nh\ni\nj\n");
        assert_eq!((d.added, d.removed), (1, 1));
        assert_eq!(d.hunks.len(), 1);
        let lines: Vec<String> = d.hunks[0].lines.iter().map(|(m, l)| format!("{m}{l}")).collect();
        // 3 context, change, 3 context.
        assert_eq!(lines.len(), 8);
        assert!(lines.contains(&"-d".to_string()));
        assert!(lines.contains(&"+D".to_string()));
        assert_eq!(d.hunks[0].old_start, 1);
        assert_eq!(d.hunks[0].new_start, 1);
    }

    #[test]
    fn pure_addition_opens_at_next_old_line() {
        let d = diff_blobs(b"one\n", b"one\ntwo\n");
        assert_eq!(d.added, 1);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].old_start, 1);
        assert_eq!(d.hunks[0].new_start, 1);
    }

    #[test]
    fn deletion_only() {
        let d = diff_blobs(b"a\nb\nc\n", b"a\nc\n");
        assert_eq!(d.removed, 1);
        assert_eq!(d.hunks.len(), 1);
        assert!(d.hunks[0].lines.iter().any(|(m, l)| (*m, l.as_str()) == ('-', "b")));
    }

    #[test]
    fn binary_detected_via_nul() {
        let d = diff_blobs(b"png\x00data", b"png\x00data2");
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn separate_edits_make_separate_hunks() {
        let old: Vec<String> = (1..=20).map(|i| format!("line{i}")).collect();
        let mut new = old.clone();
        new[4] = "X".to_string();
        new[19] = "Y".to_string();
        let d = diff_blobs(old.join("\n").as_bytes(), new.join("\n").as_bytes());
        assert_eq!(d.added, 2);
        assert_eq!(d.removed, 2);
        assert_eq!(d.hunks.len(), 2);
    }

    #[test]
    fn budget_exceeded_collapses() {
        let old: Vec<String> = (0..3000).map(|i| format!("old line {i} aaaaaaaa")).collect();
        let new: Vec<String> = (0..3000).map(|i| format!("new line {i} bbbbbbbb")).collect();
        let d = diff_blobs(old.join("\n").as_bytes(), new.join("\n").as_bytes());
        assert!(!d.binary);
        assert_eq!(d.added, 3000);
        assert_eq!(d.removed, 3000);
    }

    #[test]
    fn empty_to_content_is_single_add() {
        let d = diff_blobs(b"", b"hello\nworld\n");
        assert_eq!(d.added, 2);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].old_start, 0);
        assert_eq!(d.hunks[0].new_start, 1);
    }
}

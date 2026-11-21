//! Typed unified diff generation for checkpoint previews.
//!
//! One reviewed crate (`similar`) owns the diff algorithm. The runtime only
//! shapes the result into bounded, typed hunks and never exposes raw snapshot
//! bytes or host paths.

use common::{DiffHunk, DiffLine, FileDiff};
use similar::{ChangeTag, TextDiff};

/// Maximum hunks returned for one file.
const MAX_HUNKS: usize = 50;
/// Maximum diff lines returned for one file.
const MAX_LINES: usize = 4_000;
/// Context lines around each change cluster.
const CONTEXT: usize = 3;

/// Structural change for one file.
pub fn change(before: &crate::BlobRef, after: &crate::BlobRef) -> &'static str {
    match (before.present, after.present) {
        (false, true) => "added",
        (true, false) => "deleted",
        (true, true) if before.digest == after.digest => "unchanged",
        _ => "modified",
    }
}

/// Build one typed file diff. Returns `None` when the content is unchanged.
pub fn file_diff(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Option<FileDiff> {
    let change = match (before.is_some(), after.is_some()) {
        (false, true) => "added",
        (true, false) => "deleted",
        (false, false) => return None,
        (true, true) => {
            if before == after {
                return None;
            }
            "modified"
        }
    };
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let binary = is_binary(before) || is_binary(after);
    if binary {
        return Some(FileDiff {
            path: path.to_string(),
            change: change.to_string(),
            additions: 0,
            deletions: 0,
            binary: true,
            hunks: Vec::new(),
        });
    }
    let old = String::from_utf8_lossy(before);
    let new = String::from_utf8_lossy(after);
    let diff = TextDiff::from_lines(old.as_ref(), new.as_ref());
    let (lines, _truncated) = hunks(&diff);
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for hunk in &lines {
        for line in &hunk.lines {
            if line.kind == "add" {
                additions += 1;
            } else if line.kind == "remove" {
                deletions += 1;
            }
        }
    }
    Some(FileDiff {
        path: path.to_string(),
        change: change.to_string(),
        additions,
        deletions,
        binary: false,
        hunks: lines,
    })
}

fn hunks(diff: &TextDiff<'_, '_, '_, str>) -> (Vec<DiffHunk>, bool) {
    let mut hunks = Vec::new();
    let mut lines = 0usize;
    let mut truncated = false;
    for group in diff.grouped_ops(CONTEXT) {
        if hunks.len() >= MAX_HUNKS || lines >= MAX_LINES {
            truncated = true;
            break;
        }
        let Some(first) = group.first() else { continue };
        let old_start = first.old_range().start + 1;
        let new_start = first.new_range().start + 1;
        let mut old_lines = 0usize;
        let mut new_lines = 0usize;
        let mut hunk_lines = Vec::new();
        'group: for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => "context",
                    ChangeTag::Delete => "remove",
                    ChangeTag::Insert => "add",
                };
                if lines >= MAX_LINES {
                    truncated = true;
                    break 'group;
                }
                if change.tag() != ChangeTag::Insert {
                    old_lines += 1;
                }
                if change.tag() != ChangeTag::Delete {
                    new_lines += 1;
                }
                hunk_lines.push(DiffLine {
                    kind: kind.to_string(),
                    text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                });
                lines += 1;
            }
        }
        hunks.push(DiffHunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: hunk_lines,
        });
    }
    (hunks, truncated)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_file_has_one_insert_hunk() {
        let diff = file_diff("a.txt", None, Some(b"alpha\nbeta\n")).expect("diff");
        assert_eq!(diff.change, "added");
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.deletions, 0);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].old_lines, 0);
        assert_eq!(diff.hunks[0].new_start, 1);
        assert!(diff.hunks[0].lines.iter().all(|line| line.kind == "add"));
    }

    #[test]
    fn modified_file_reports_context_adds_and_removes() {
        let diff = file_diff("a.txt", Some(b"a\nb\nc\n"), Some(b"a\nB\nc\n")).expect("diff");
        assert_eq!(diff.change, "modified");
        assert_eq!(diff.additions, 1);
        assert_eq!(diff.deletions, 1);
        assert!(diff.hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == "context"));
    }

    #[test]
    fn unchanged_content_has_no_diff() {
        assert!(file_diff("a.txt", Some(b"same\n"), Some(b"same\n")).is_none());
    }

    #[test]
    fn binary_content_is_typed_without_hunks() {
        let diff = file_diff("a.bin", None, Some(&[0, 1, 2, 3])).expect("diff");
        assert!(diff.binary);
        assert!(diff.hunks.is_empty());
    }
}

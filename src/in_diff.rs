// Copyright 2023-2026 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! `--in-diff`: keep the mutants whose span touches a line that a diff adds or
//! changes. Changes to files that are not Rust source are ignored.

use std::collections::HashMap;
use std::fmt::Display;
use std::io::Read;

use camino::Utf8Path;
use flickzeug::{Diff, Line};
use itertools::Itertools;

use crate::mutant::Mutant;

/// Why a diff could not be used, or left nothing to test.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DiffFilterError {
    /// The diff is empty.
    EmptyDiff,
    /// The new text of the diff is not what the source tree has.
    MismatchedDiff(String),
    /// The diff is not empty but touches no mutant.
    NoMutants,
    /// The diff is not empty but changes no Rust source file.
    NoSourceFiles,
    /// The diff cannot be parsed.
    InvalidDiff(String),
    /// The diff file cannot be read.
    File(String),
}

impl DiffFilterError {
    /// The exit code, those of cargo-mutants: an empty diff, or one that leaves
    /// no mutant, is not a failure (0); a diff that is out of date is 5; one
    /// that cannot be read is 6.
    pub fn exit_code(&self) -> i32 {
        match self {
            DiffFilterError::EmptyDiff
            | DiffFilterError::NoSourceFiles
            | DiffFilterError::NoMutants => 0,
            DiffFilterError::MismatchedDiff(_) => 5,
            DiffFilterError::InvalidDiff(_) | DiffFilterError::File(_) => 6,
        }
    }
}

impl Display for DiffFilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffFilterError::EmptyDiff => write!(f, "Diff file is empty"),
            DiffFilterError::NoSourceFiles => write!(f, "Diff changes no Rust source files"),
            DiffFilterError::NoMutants => write!(f, "No mutants to filter"),
            DiffFilterError::MismatchedDiff(msg) => write!(f, "{msg}"),
            DiffFilterError::InvalidDiff(msg) => write!(f, "Failed to parse diff: {msg}"),
            DiffFilterError::File(msg) => write!(f, "Failed to read diff file: {msg}"),
        }
    }
}

impl std::error::Error for DiffFilterError {}

/// Read a diff from a file, or from standard input for `-`, and filter the mutants.
pub fn diff_filter_file(
    mutants: Vec<Mutant>,
    diff_path: &Utf8Path,
) -> Result<Vec<Mutant>, DiffFilterError> {
    let mut bytes = Vec::new();
    let read = if diff_path == "-" {
        std::io::stdin().lock().read_to_end(&mut bytes)
    } else {
        std::fs::File::open(diff_path).and_then(|mut f| f.read_to_end(&mut bytes))
    };
    read.map_err(|err| DiffFilterError::File(format!("{diff_path}: {err}")))?;
    // The diff may hold non-UTF-8 text in files that are not Rust source,
    // and Rust source is UTF-8, so what cannot be decoded does not matter.
    diff_filter(mutants, &String::from_utf8_lossy(&bytes))
}

/// Filter a list of mutants to those intersecting a diff on the file tree.
pub fn diff_filter(mutants: Vec<Mutant>, diff_text: &str) -> Result<Vec<Mutant>, DiffFilterError> {
    let patches = match flickzeug::patch_from_str(diff_text) {
        Ok(patches) => patches,
        Err(err) => return Err(DiffFilterError::InvalidDiff(err.to_string())),
    };
    if patches.is_empty() {
        return Err(DiffFilterError::EmptyDiff);
    }
    if let Err(msg) = check_diff_new_text_matches(&patches, &mutants) {
        return Err(DiffFilterError::MismatchedDiff(msg));
    }
    let mut lines_changed_by_path: HashMap<&Utf8Path, Vec<usize>> = HashMap::new();
    let mut changed_rs_file = false;
    for patch in &patches {
        let path = strip_patch_path(patch.modified().unwrap_or_default());
        if path != "/dev/null" && path.extension() == Some("rs") {
            changed_rs_file = true;
            lines_changed_by_path
                .entry(path)
                .or_default()
                .extend(affected_lines(patch));
        }
    }
    for lines in lines_changed_by_path.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }
    let matched: Vec<Mutant> = mutants
        .into_iter()
        .filter(|mutant| {
            lines_changed_by_path
                .get(mutant.file.as_path())
                .is_some_and(|lines| {
                    (mutant.span.start.line..=mutant.span.end.line)
                        .any(|line| lines.binary_search(&line).is_ok())
                })
        })
        .collect();
    if matched.is_empty() {
        if changed_rs_file {
            Err(DiffFilterError::NoMutants)
        } else {
            Err(DiffFilterError::NoSourceFiles)
        }
    } else {
        Ok(matched)
    }
}

/// Check that the "new" side of the diff matches the source in this tree: if
/// not, the lines in the diff mean nothing. The message names the first line
/// that differs.
fn check_diff_new_text_matches(diffs: &[Diff<str>], mutants: &[Mutant]) -> Result<(), String> {
    let mut source_by_name: HashMap<&Utf8Path, &Mutant> = HashMap::new();
    for mutant in mutants {
        source_by_name
            .entry(mutant.file.as_path())
            .or_insert(mutant);
    }
    for diff in diffs {
        let Some(path) = diff.modified() else {
            continue;
        };
        let path = strip_patch_path(path);
        if let Some(mutant) = source_by_name.get(&path) {
            let lines = mutant.code.lines().collect_vec();
            for (lineno, diff_content) in partial_new_file(diff) {
                let source_content = lines.get(lineno - 1).unwrap_or(&"");
                if diff_content != *source_content {
                    return Err(format!(
                        "Diff content doesn't match source file: {path} line {lineno}\n\
                         diff has:   {diff_content:?}\n\
                         source has: {source_content:?}\n\
                         The diff might be out of date with this source tree.\n"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Remove the `b/` prefix commonly found in paths within diffs.
fn strip_patch_path(path: &str) -> &Utf8Path {
    let path = Utf8Path::new(path);
    path.strip_prefix("b").unwrap_or(path)
}

/// The lines of the new file that a diff adds, or that are next to a deletion
/// (the line before and the line after it); context lines are not included.
fn affected_lines(diff: &Diff<str>) -> Vec<usize> {
    let mut affected_lines: Vec<usize> = Vec::new();
    for hunk in diff.hunks() {
        let mut lineno: usize = hunk.new_range().start();
        // True if the previous line was deleted: the next line that exists in
        // the new file, if there is one, is affected.
        let mut prev_removed = false;
        for line in hunk.lines() {
            match line {
                Line::Delete(_) => {
                    prev_removed = true;
                }
                Line::Insert(_) | Line::Context(_) => {
                    if prev_removed {
                        affected_lines.push(lineno);
                        prev_removed = false;
                    }
                }
            }
            match line {
                Line::Context(_) => {
                    lineno += 1;
                }
                Line::Insert(_) => {
                    if affected_lines.last().is_none_or(|last| *last != lineno) {
                        affected_lines.push(lineno);
                    }
                    lineno += 1;
                }
                Line::Delete(_) => {
                    if lineno > 1
                        && affected_lines
                            .last()
                            .is_none_or(|last| *last != (lineno - 1))
                    {
                        affected_lines.push(lineno - 1);
                    }
                }
            }
        }
    }
    affected_lines
}

/// The part of the new file that a diff shows: its added and context lines.
fn partial_new_file<'d>(diff: &'d Diff<'d, str>) -> Vec<(usize, &'d str)> {
    let mut r: Vec<(usize, &'d str)> = Vec::new();
    for hunk in diff.hunks() {
        let mut lineno: usize = hunk.new_range().start();
        for line in hunk.lines() {
            match line {
                Line::Context((text, _)) | Line::Insert((text, _)) => {
                    r.push((lineno, text));
                    lineno += 1;
                }
                Line::Delete(_) => {}
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(diff: &str) -> Vec<usize> {
        let patches = flickzeug::patch_from_str(diff).unwrap();
        affected_lines(&patches[0])
    }

    #[test]
    fn added_lines_and_neighbours_of_deletions_are_affected() {
        // line 3 added; context is not affected
        let d = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,4 @@\n a\n b\n+c\n d\n";
        assert_eq!(lines_of(d), [3]);
        // line 3 of the old file deleted: the line before and the line after
        let d = "--- a/x.rs\n+++ b/x.rs\n@@ -1,4 +1,3 @@\n a\n b\n-c\n d\n";
        assert_eq!(lines_of(d), [2, 3]);
        // a deletion at the start of the file has no line before it
        let d = "--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,1 @@\n-a\n b\n";
        assert_eq!(lines_of(d), [1]);
        // a changed line: the one before it counts as well
        let d = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        assert_eq!(lines_of(d), [1, 2]);
    }

    #[test]
    fn strips_the_b_prefix() {
        assert_eq!(strip_patch_path("b/src/a.rs"), "src/a.rs");
        assert_eq!(strip_patch_path("src/a.rs"), "src/a.rs");
        assert_eq!(strip_patch_path("bb/a.rs"), "bb/a.rs");
    }

    #[test]
    fn errors_of_unusable_diffs() {
        assert_eq!(
            diff_filter(Vec::new(), "").unwrap_err(),
            DiffFilterError::EmptyDiff
        );
        assert_eq!(
            diff_filter(Vec::new(), "not really a diff\n").unwrap_err(),
            DiffFilterError::EmptyDiff
        );
        let readme = "--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-a\n+b\n";
        let e = diff_filter(Vec::new(), readme).unwrap_err();
        assert_eq!(e, DiffFilterError::NoSourceFiles);
        let rs = "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let e = diff_filter(Vec::new(), rs).unwrap_err();
        assert_eq!(e, DiffFilterError::NoMutants);
        assert_eq!(e.to_string(), "No mutants to filter");
        let codes: Vec<i32> = [
            DiffFilterError::EmptyDiff,
            DiffFilterError::NoMutants,
            DiffFilterError::NoSourceFiles,
            DiffFilterError::MismatchedDiff(String::new()),
            DiffFilterError::InvalidDiff(String::new()),
            DiffFilterError::File(String::new()),
        ]
        .iter()
        .map(DiffFilterError::exit_code)
        .collect();
        assert_eq!(codes, [0, 0, 0, 5, 6, 6]);
    }
}

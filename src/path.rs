// Copyright 2022-2025 Martin Pool.
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! Utilities for file paths.

use std::cmp::max;

use camino::Utf8Path;

/// Measures how far above its starting point a path ascends.
///
/// This only considers the textual content of the path.
pub fn ascent(path: &Utf8Path) -> isize {
    let mut max_ascent: isize = 0;
    let mut ascent = 0;
    for component in path.components().map(|c| c.as_str()) {
        match component {
            ".." => {
                ascent += 1;
                max_ascent = max(ascent, max_ascent);
            }
            "." => (),
            _ => ascent -= 1,
        }
    }
    max_ascent
}

/// The path with forward slashes, even on Windows.
pub fn to_slash_path(path: &Utf8Path) -> String {
    path.components()
        .map(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(|c| if c == "/" || c == "\\" { "" } else { c })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod test {
    use camino::Utf8Path;

    use super::{ascent, to_slash_path};

    #[test]
    fn ascent_counts_parent_steps() {
        assert_eq!(ascent(Utf8Path::new("src/lib.rs")), 0);
        assert_eq!(ascent(Utf8Path::new("../a/b.rs")), 1);
        assert_eq!(ascent(Utf8Path::new("a/../../b.rs")), 1);
    }

    #[test]
    fn slash_path_joins_components() {
        assert_eq!(to_slash_path(Utf8Path::new("src/a/b.rs")), "src/a/b.rs");
    }
}

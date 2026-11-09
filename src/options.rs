// Copyright 2022-2026 Martin Pool.
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! Options that decide which mutants are listed, merged from the command line
//! and `.cargo/mutants.toml`, and the glob handling they use.

use std::borrow::Cow;
use std::fs::read_to_string;
use std::path::Path;

use anyhow::{Context, Result};
use camino::Utf8Path;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexSet;
use serde::Deserialize;
use syn::Expr;

use crate::mutant::Mutant;

/// The subset of the `.cargo/mutants.toml` configuration that affects listing.
///
/// Keys that only matter for building and running tests are accepted and ignored.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Generate these error values from functions returning Result.
    pub error_values: Vec<String>,
    /// Generate mutants from source files matching these globs.
    pub examine_globs: Vec<String>,
    /// Examine only mutants whose name matches one of these regexps.
    pub examine_re: Vec<String>,
    /// Exclude mutants from source files matching these globs.
    pub exclude_globs: Vec<String>,
    /// Exclude mutants whose name matches these regexps.
    pub exclude_re: Vec<String>,
    /// Skip calls to functions or methods with these names.
    pub skip_calls: Vec<String>,
    /// Use built-in defaults for `skip_calls` in addition to any explicit values.
    pub skip_calls_defaults: Option<bool>,
}

impl Config {
    pub fn read_file(path: &Path) -> Result<Config> {
        let toml =
            read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        toml::de::from_str(&toml).with_context(|| format!("parse toml from {}", path.display()))
    }

    /// Read the config from a tree's `.cargo/mutants.toml`, and return a default (empty)
    /// Config if the file does not exist.
    pub fn read_tree_config(workspace_dir: &Utf8Path) -> Result<Config> {
        let path = workspace_dir.join(".cargo").join("mutants.toml");
        if path.exists() {
            Config::read_file(path.as_ref())
        } else {
            Ok(Config::default())
        }
    }
}

/// The command-line side of the filter options.
#[derive(Debug, Default, Clone)]
pub struct Filters {
    pub error: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub exclude_re: Vec<String>,
    pub examine_globs: Vec<String>,
    pub examine_re: Vec<String>,
    pub skip_calls: Vec<String>,
    pub skip_calls_defaults: Option<bool>,
}

#[derive(Debug)]
pub struct Options {
    /// Files to examine.
    pub examine_globset: Option<GlobSet>,
    /// Files to exclude.
    pub exclude_globset: Option<GlobSet>,
    /// Mutants to examine, as a regexp matched against the full name.
    pub examine_name_re: RegexSet,
    /// Mutants to exclude, as a regexp matched against the full name.
    pub exclude_name_re: RegexSet,
    /// Skip calls to functions or methods with these names.
    pub skip_calls: Vec<String>,
    /// Error values for functions returning `Result`.
    pub error_values: Vec<String>,
}

impl Options {
    /// Combine command line arguments with the config file: lists are
    /// concatenated (command line first), scalars prefer the command line.
    pub fn new(args: &Filters, config: &Config) -> Result<Options> {
        let mut skip_calls: Vec<String> = args
            .skip_calls
            .iter()
            .flat_map(|s| s.split(','))
            .map(ToString::to_string)
            .chain(config.skip_calls.iter().cloned())
            .collect();
        if args
            .skip_calls_defaults
            .or(config.skip_calls_defaults)
            .unwrap_or(true)
        {
            skip_calls.push("with_capacity".to_owned());
        }
        Ok(Options {
            error_values: args
                .error
                .iter()
                .chain(&config.error_values)
                .cloned()
                .collect(),
            examine_name_re: RegexSet::new(args.examine_re.iter().chain(config.examine_re.iter()))
                .context("Failed to compile examine_re regex")?,
            exclude_name_re: RegexSet::new(args.exclude_re.iter().chain(&config.exclude_re))
                .context("Failed to compile exclude_re regex")?,
            examine_globset: build_glob_set(
                args.examine_globs.iter().chain(config.examine_globs.iter()),
            )?,
            exclude_globset: build_glob_set(
                args.exclude_globs.iter().chain(config.exclude_globs.iter()),
            )?,
            skip_calls,
        })
    }

    /// Return the syn ASTs for the error values, which should be inserted as return values
    /// from functions returning `Result`.
    pub fn parsed_error_exprs(&self) -> Result<Vec<Expr>> {
        self.error_values
            .iter()
            .map(|e| {
                syn::parse_str(e).with_context(|| format!("Failed to parse error value {e:?}"))
            })
            .collect()
    }

    /// True if the options allow mutants to be generated from the given path.
    ///
    /// That is: it matches the examine globset (if specified) and does not match the exclude globset
    /// (if specified).
    pub fn allows_source_file_path(&self, path: &Utf8Path) -> bool {
        self.examine_globset
            .as_ref()
            .is_none_or(|g| g.is_match(path))
            && !self
                .exclude_globset
                .as_ref()
                .is_some_and(|g| g.is_match(path))
    }

    /// True if the options allow this mutant to be listed.
    pub fn allows_mutant(&self, mutant: &Mutant) -> bool {
        let name = mutant.name(true);
        (self.examine_name_re.is_empty() || self.examine_name_re.is_match(&name))
            && (self.exclude_name_re.is_empty() || !self.exclude_name_re.is_match(&name))
    }
}

/// Build a globset from patterns: a pattern without a slash matches a file or
/// directory name anywhere, one with a slash matches the whole path.
pub fn build_glob_set<S, I>(globs: I) -> Result<Option<GlobSet>>
where
    S: AsRef<str>,
    I: IntoIterator<Item = S>,
{
    let mut has_globs = false;
    let mut builder = GlobSetBuilder::new();
    for glob_str in globs {
        has_globs = true;
        let glob_str = glob_str.as_ref();
        let adjusted = if glob_str.contains('/') {
            vec![Cow::Borrowed(glob_str)]
        } else {
            vec![
                Cow::Owned(format!("**/{glob_str}")),
                Cow::Owned(format!("**/{glob_str}/**")),
            ]
        };
        for g in adjusted {
            builder.add(
                GlobBuilder::new(&g)
                    .literal_separator(true) // * does not match /
                    .build()
                    .with_context(|| format!("Failed to build glob from {glob_str:?}"))?,
            );
        }
    }
    if has_globs {
        Ok(Some(builder.build().context("Failed to build glob set")?))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn matches(globs: &[&str], path: &str) -> bool {
        build_glob_set(globs).unwrap().unwrap().is_match(path)
    }

    #[test]
    fn no_globs_builds_no_set() {
        assert!(build_glob_set(&[] as &[&str]).unwrap().is_none());
    }

    #[test]
    fn glob_without_slash_matches_anywhere() {
        assert!(matches(&["foo.rs"], "src/a/foo.rs"));
        assert!(matches(&["foo"], "src/foo/bar.rs"));
        assert!(!matches(&["foo.rs"], "src/a/barfoo.rs"));
    }

    #[test]
    fn glob_with_slash_matches_whole_path() {
        assert!(matches(&["src/*.rs"], "src/a.rs"));
        assert!(!matches(&["src/*.rs"], "src/x/a.rs"));
        assert!(matches(&["**/a.rs"], "src/x/a.rs"));
    }

    #[test]
    fn merges_command_line_and_config() {
        let args = Filters {
            exclude_re: vec!["foo".into()],
            examine_globs: vec!["a.rs".into()],
            skip_calls: vec!["x,y".into()],
            ..Filters::default()
        };
        let config: Config = toml::de::from_str(
            r#"
            exclude_re = ["bar"]
            examine_globs = ["b.rs"]
            skip_calls = ["z"]
            skip_calls_defaults = false
            some_other_key = 1
            "#,
        )
        .unwrap();
        let o = Options::new(&args, &config).unwrap();
        assert!(o.exclude_name_re.is_match("foo") && o.exclude_name_re.is_match("bar"));
        assert!(o.allows_source_file_path(Utf8Path::new("src/b.rs")));
        assert!(!o.allows_source_file_path(Utf8Path::new("src/c.rs")));
        assert_eq!(o.skip_calls, ["x", "y", "z"]);
        let d = Options::new(&Filters::default(), &Config::default()).unwrap();
        assert_eq!(d.skip_calls, ["with_capacity"]);
    }
}

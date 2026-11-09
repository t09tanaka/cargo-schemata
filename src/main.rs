//! cargo-schemata: mutation testing for Rust that builds mutants once, as schemata.
//!
//! This stage lists the mutants; rewriting, building and running them come later.

mod fnvalue;
mod list;
mod mutant;
mod options;
mod path;
mod pretty;
mod span;
mod visit;
mod workspace;

use std::process::ExitCode;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;

use crate::options::{Config, Filters, Options};
use crate::workspace::{PackageFilter, Workspace};

/// Mutation testing for Rust that builds mutants once, as schemata.
#[derive(Parser, Debug)]
#[command(name = "cargo-schemata", version, bin_name = "cargo schemata")]
struct Args {
    /// Just list possible mutants, don't run them.
    #[arg(long, help_heading = "Output")]
    list: bool,

    /// Output json (only for --list).
    #[arg(long, help_heading = "Output")]
    json: bool,

    /// Glob for files to examine; with no glob, all files are examined. Globs
    /// containing a slash match the whole path.
    #[arg(long, short = 'f', help_heading = "Filters")]
    file: Vec<String>,

    /// Glob for files to exclude; applied after `--file`.
    #[arg(long, short = 'e', help_heading = "Filters")]
    exclude: Vec<String>,

    /// Regex for mutants to examine, matched against the names shown by `--list`.
    #[arg(
        long = "re",
        short = 'F',
        alias = "regex",
        alias = "examine-regex",
        alias = "examine-re",
        help_heading = "Filters"
    )]
    examine_re: Vec<String>,

    /// Regex for mutants to exclude, matched against the names shown by `--list`.
    #[arg(long, short = 'E', alias = "exclude-regex", help_heading = "Filters")]
    exclude_re: Vec<String>,

    /// Mutate only this package; may be repeated.
    #[arg(long = "package", short = 'p', help_heading = "Filters")]
    packages: Vec<String>,

    /// Mutate every package in the workspace.
    #[arg(long, help_heading = "Filters")]
    workspace: bool,

    /// Skip calls to functions and methods named in this list (comma-separated, repeatable).
    #[arg(long, help_heading = "Filters")]
    skip_calls: Vec<String>,

    /// Also skip the built-in default calls (`with_capacity`).
    #[arg(long, action = clap::ArgAction::Set, help_heading = "Filters")]
    skip_calls_defaults: Option<bool>,

    /// Return this error value from functions returning Result: for example,
    /// `::anyhow::anyhow!("mutated")`.
    #[arg(long, help_heading = "Generate")]
    error: Vec<String>,

    /// Rust crate directory to examine.
    #[arg(
        long,
        short = 'd',
        conflicts_with = "manifest_path",
        help_heading = "Input"
    )]
    dir: Option<Utf8PathBuf>,

    /// Path to Cargo.toml for the package to mutate.
    #[arg(long, help_heading = "Input")]
    manifest_path: Option<Utf8PathBuf>,

    /// Read this configuration file instead of `.cargo/mutants.toml`.
    #[arg(long, conflicts_with = "no_config", help_heading = "Config")]
    config: Option<Utf8PathBuf>,

    /// Don't read `.cargo/mutants.toml`.
    #[arg(long, help_heading = "Config")]
    no_config: bool,

    /// Arguments for the test runner. Accepted for compatibility; they do not
    /// affect `--list`.
    #[arg(last = true)]
    test_args: Vec<String>,
}

/// Drops the `schemata` token cargo inserts when invoked as `cargo schemata`.
fn strip_subcommand(args: Vec<String>) -> Vec<String> {
    let mut it = args.into_iter();
    let mut out: Vec<String> = it.next().into_iter().collect();
    let rest: Vec<String> = it.collect();
    let skip = usize::from(rest.first().map(String::as_str) == Some("schemata"));
    out.extend(rest.into_iter().skip(skip));
    out
}

fn run(args: &Args) -> Result<String> {
    let start_dir: &Utf8Path = if let Some(manifest_path) = &args.manifest_path {
        manifest_path
            .parent()
            .filter(|p| !p.as_str().is_empty())
            .unwrap_or(Utf8Path::new("."))
    } else if let Some(dir) = &args.dir {
        dir
    } else {
        Utf8Path::new(".")
    };
    let workspace = Workspace::open(start_dir)?;
    let config = if args.no_config {
        Config::default()
    } else if let Some(config_path) = &args.config {
        Config::read_file(config_path.as_ref())?
    } else {
        Config::read_tree_config(workspace.root())?
    };
    let options = Options::new(
        &Filters {
            error: args.error.clone(),
            exclude_globs: args.exclude.clone(),
            exclude_re: args.exclude_re.clone(),
            examine_globs: args.file.clone(),
            examine_re: args.examine_re.clone(),
            skip_calls: args.skip_calls.clone(),
            skip_calls_defaults: args.skip_calls_defaults,
        },
        &config,
    )?;
    let package_filter = if !args.packages.is_empty() {
        PackageFilter::Explicit(args.packages.clone())
    } else if args.workspace {
        PackageFilter::All
    } else {
        PackageFilter::Auto(start_dir.to_owned())
    };
    let packages = workspace.select(&package_filter)?;
    let mutants = visit::walk_tree(workspace.root(), &packages, &options)
        .context("failed to discover mutants")?;
    Ok(list::list_mutants(&mutants, args.json))
}

fn main() -> ExitCode {
    let args = Args::parse_from(strip_subcommand(std::env::args().collect()));
    if !args.list {
        eprintln!("error: only `--list` is supported so far; pass --list");
        return ExitCode::from(2);
    }
    if args.json && !args.list {
        return ExitCode::from(2);
    }
    match run(&args) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn handles_cargo_subcommand_invocation() {
        let args = strip_subcommand(v(&["cargo-schemata", "schemata", "--list"]));
        assert_eq!(args, v(&["cargo-schemata", "--list"]));
        assert!(Args::try_parse_from(args).unwrap().list);
    }

    #[test]
    fn parses_filters_and_test_args() {
        let a = Args::try_parse_from(v(&[
            "x",
            "--list",
            "--json",
            "--file",
            "a.rs",
            "--file",
            "b.rs",
            "--re",
            "foo",
            "--exclude-re",
            "bar",
            "-d",
            "dir",
            "--",
            "--lib",
        ]))
        .unwrap();
        assert_eq!(a.file, v(&["a.rs", "b.rs"]));
        assert_eq!(a.examine_re, v(&["foo"]));
        assert_eq!(a.exclude_re, v(&["bar"]));
        assert_eq!(a.dir.as_deref(), Some(Utf8Path::new("dir")));
        assert_eq!(a.test_args, v(&["--lib"]));
    }

    #[test]
    fn rejects_out_of_scope_options() {
        for opt in ["--in-diff=x", "--shard=1/2", "-j2"] {
            assert!(
                Args::try_parse_from(v(&["x", "--list", opt])).is_err(),
                "{opt}"
            );
        }
    }
}

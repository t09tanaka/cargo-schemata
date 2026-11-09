//! cargo-schemata: mutation testing for Rust that builds mutants once, as schemata.
//!
//! `--list` lists the mutants, `--build-only` builds them, and a plain run builds
//! them, runs the tests of each and reports what was caught.

mod diag;
mod exec;
mod fnvalue;
mod interrupt;
mod list;
mod lock;
mod mutant;
mod options;
mod path;
mod prepare;
mod pretty;
mod proc;
mod report;
mod results;
mod rewrite;
mod route;
mod span;
mod sync;
mod visit;
mod workspace;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;

use crate::exec::{Baseline, Ran, Tests};
use crate::interrupt::Interrupted;
use crate::mutant::Mutant;
use crate::options::{Config, Filters, Options};
use crate::prepare::Single;
use crate::results::{Counts, MutantResult, Status};
use crate::workspace::{Package, PackageFilter, Workspace};

/// Mutation testing for Rust that builds mutants once, as schemata.
#[derive(Parser, Debug)]
#[command(name = "cargo-schemata", version, bin_name = "cargo schemata")]
struct Args {
    /// Just list possible mutants, don't run them.
    #[arg(long, conflicts_with_all = ["jobs", "timeout"], help_heading = "Output")]
    list: bool,

    /// Rewrite and build the mutants, and report how each was built, without running tests.
    #[arg(long, conflicts_with = "list", help_heading = "Output")]
    build_only: bool,

    /// Output json: the report (for a run), the list, or the build report.
    #[arg(long, help_heading = "Output")]
    json: bool,

    /// Write `report.json` and the `caught.txt`, `missed.txt`, `timeout.txt` and
    /// `unviable.txt` lists to this directory.
    #[arg(
        long,
        short = 'o',
        default_value = "schemata.out",
        help_heading = "Output"
    )]
    output: Utf8PathBuf,

    /// Run this many test processes at a time. Default: a quarter of the
    /// processors, at least 1 and at most 4.
    #[arg(long, short = 'j', value_parser = clap::value_parser!(u32).range(1..), help_heading = "Execution")]
    jobs: Option<u32>,

    /// Run every test for every mutant. By default each mutant runs only the
    /// tests that reach it (found by running each test alone once), and ends as
    /// soon as a test fails.
    #[arg(long, help_heading = "Execution")]
    no_routing: bool,

    /// Seconds one mutant's tests may take. Default: five times the time of
    /// the tests without a mutant, at least 20.
    #[arg(long, short = 't', value_parser = parse_timeout, help_heading = "Execution")]
    timeout: Option<f64>,

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

    /// Arguments for `cargo test` such as `--lib` (as in cargo-mutants); a second
    /// `--` starts the arguments of the test binaries. They do not affect `--list`.
    #[arg(last = true)]
    test_args: Vec<String>,
}

fn parse_timeout(s: &str) -> Result<f64, String> {
    let secs: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if secs.is_finite() && secs > 0.0 {
        Ok(secs)
    } else {
        Err("must be a positive number of seconds".to_owned())
    }
}

/// What follows `--`: arguments for cargo, and for the test binaries.
#[derive(Debug, Default, PartialEq, Eq)]
struct TestArgs {
    /// For `cargo check` and `cargo test --no-run`: target selection, features, profile.
    build: Vec<String>,
    /// For every test binary: a test name filter, and what follows a second `--`.
    binary: Vec<String>,
}

/// Options of `cargo test` that take a value in the next argument.
const VALUE_OPTIONS: &[&str] = &[
    "--bin",
    "--example",
    "--test",
    "--bench",
    "--package",
    "-p",
    "--exclude",
    "--features",
    "-F",
    "--target",
    "--profile",
    "--jobs",
    "-j",
    "--manifest-path",
    "--color",
    "--config",
    "-Z",
    "--target-dir",
    "--message-format",
    "--lockfile-path",
];

/// Options of `cargo test` that `cargo check` and `cargo test --no-run` do not
/// take, or that this program sets itself.
const NOT_FOR_BUILD: &[&str] = &[
    "--no-fail-fast",
    "--doc",
    "--no-run",
    "--target-dir",
    "--message-format",
];

fn split_test_args(args: &[String]) -> TestArgs {
    let (cargo_part, binary_part) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &args[args.len()..]),
    };
    let mut out = TestArgs::default();
    let mut it = cargo_part.iter();
    while let Some(a) = it.next() {
        if !a.starts_with('-') {
            // as for `cargo test`, a bare word is a test name filter
            out.binary.push(a.clone());
            continue;
        }
        let name = a.split('=').next().unwrap_or(a);
        let takes_value = VALUE_OPTIONS.contains(&a.as_str());
        let value = if takes_value { it.next() } else { None };
        if NOT_FOR_BUILD.contains(&name) {
            continue;
        }
        out.build.push(a.clone());
        out.build.extend(value.cloned());
    }
    out.binary.extend(binary_part.iter().cloned());
    out
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

struct Setup {
    workspace: Workspace,
    packages: Vec<Arc<Package>>,
    mutants: Vec<Mutant>,
}

fn discover(args: &Args) -> Result<Setup> {
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
    Ok(Setup {
        workspace,
        packages,
        mutants,
    })
}

fn build_options(args: &Args) -> prepare::BuildOptions {
    prepare::BuildOptions {
        cargo_args: split_test_args(&args.test_args).build,
        ..Default::default()
    }
}

fn schemata_dir(workspace: &Workspace) -> std::path::PathBuf {
    workspace.target_dir().as_std_path().join("schemata")
}

fn run_list_or_build(args: &Args) -> Result<String> {
    let setup = discover(args)?;
    if args.build_only {
        let _lock = lock::Lock::acquire(&schemata_dir(&setup.workspace))?;
        let mut prepared = prepare::prepare(
            &setup.workspace,
            &setup.packages,
            &setup.mutants,
            &build_options(args),
        )
        .context("failed to build mutants")?;
        prepare::build_singles(&mut prepared, &setup.mutants).context("failed to build mutants")?;
        return Ok(report::report(&setup.mutants, &prepared, args.json));
    }
    Ok(list::list_mutants(&setup.mutants, args.json))
}

/// Why a mutant was missed without running any test.
const NOT_REACHED: &str = "not reached by any test";

fn status_of(ran: Ran) -> Status {
    match ran {
        Ran::Caught => Status::Caught,
        Ran::Missed | Ran::Unreached => Status::Missed,
        Ran::Timeout => Status::Timeout,
    }
}

/// Build the mutants, run the tests of each, report. The exit code.
fn run_tests(args: &Args) -> Result<i32> {
    let started = Instant::now();
    let setup = discover(args)?;
    let Setup {
        workspace,
        packages,
        mutants,
    } = setup;
    let json = args.json;
    // With --json, stdout is the report alone.
    let say = move |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let schemata = schemata_dir(&workspace);
    let _lock = lock::Lock::acquire(&schemata)?;
    let out_dir = args.output.as_std_path();
    results::clear_output(out_dir)?;
    let tmp_root = schemata.join("tmp");
    match std::fs::remove_dir_all(&tmp_root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("remove the temporary directories of the last run"),
    }
    let names: HashMap<usize, String> = mutants.iter().map(|m| (m.id, m.name(true))).collect();
    let by_id: HashMap<usize, &Mutant> = mutants.iter().map(|m| (m.id, m)).collect();
    let explicit = args.timeout.map(Duration::from_secs_f64);
    let mut results: Vec<MutantResult> = Vec::new();
    let mut limit = explicit;

    if mutants.is_empty() {
        say("no mutants to run".to_owned());
    } else {
        let t = Instant::now();
        let mut prepared = prepare::prepare(&workspace, &packages, &mutants, &build_options(args))
            .context("failed to build mutants")?;
        say(format!(
            "built: {} mutants in one build, {} one at a time, {} unviable ({:.1}s)",
            prepared.embedded.len(),
            prepared.single_ids.len(),
            prepared.unviable.len(),
            t.elapsed().as_secs_f64()
        ));
        if prepared.test_binaries.is_empty() {
            anyhow::bail!(
                "the build produced no test binaries; is `-- <cargo args>` selecting some?"
            );
        }
        let bin_args = split_test_args(&args.test_args).binary;
        let route_ok = !args.no_routing;
        let tests = Tests {
            bin_args: &bin_args,
            tmp_root: &tmp_root,
            early_kill: route_ok && !route::changes_output(&bin_args),
        };
        let trace_file = tmp_root.join("trace-baseline.txt");
        let t = Instant::now();
        let (baseline, libtest) = match tests.baseline(
            &prepared.test_binaries,
            explicit,
            route_ok.then_some(trace_file.as_path()),
        )? {
            Baseline::Passed { took, libtest } => (took, libtest),
            Baseline::Failed { binary, log_tail } => {
                eprintln!("{log_tail}");
                eprintln!(
                    "cargo schemata: FAILED (the tests of {binary} fail without any mutant; nothing was run)"
                );
                return Ok(1);
            }
            Baseline::TimedOut(binary) => {
                eprintln!(
                    "cargo schemata: FAILED (the tests of {binary} did not end within --timeout without any mutant; nothing was run)"
                );
                return Ok(1);
            }
        };
        let jobs = args.jobs.map_or_else(exec::default_jobs, |j| j as usize);
        let timeout = exec::mutant_timeout(baseline, explicit);
        limit = Some(timeout);
        say(format!(
            "baseline: tests pass in {:.1}s; a mutant gets {}s; {jobs} job(s) ({:.1}s)",
            baseline.as_secs_f64(),
            timeout.as_secs(),
            t.elapsed().as_secs_f64()
        ));
        let routing = if route_ok && libtest.iter().any(|&l| l) {
            let t = Instant::now();
            let global = route::read_trace(&trace_file);
            if global.is_empty() {
                say("routing off: the tests of the baseline reached no mutant, so nothing was recorded"
                    .to_owned());
                None
            } else {
                match route::trace(
                    &tests,
                    &prepared.test_binaries,
                    &libtest,
                    global,
                    jobs,
                    route::trace_budget(baseline),
                    timeout,
                )? {
                    route::TraceResult::Routing(tr) => {
                        say(format!(
                            "traced: {} tests run alone, {} binar(ies) run whole ({:.1}s)",
                            tr.tests,
                            tr.whole_binaries,
                            t.elapsed().as_secs_f64()
                        ));
                        Some(tr.routing)
                    }
                    route::TraceResult::GaveUp(took) => {
                        eprintln!(
                            "cargo schemata: tracing the tests one by one took more than {:.0}s; running every test for each mutant",
                            took.as_secs_f64()
                        );
                        None
                    }
                }
            }
        } else {
            None
        };
        let t_mutants = Instant::now();

        let line = |id: usize, word: &str, took: Duration| {
            say(format!(
                "{} ... {word} in {:.1}s",
                names[&id],
                took.as_secs_f64()
            ));
        };
        for (id, why) in std::mem::take(&mut prepared.unviable) {
            line(id, Status::Unviable.word(), Duration::ZERO);
            results.push(MutantResult {
                id,
                status: Status::Unviable,
                duration: Duration::ZERO,
                reason: Some(why),
            });
        }
        let ran = exec::run_embedded(
            &tests,
            &prepared.test_binaries,
            &prepared.embedded,
            &exec::Pool {
                jobs,
                timeout,
                routing: routing.as_ref(),
            },
            &|id, ran, took| {
                if ran == Ran::Unreached {
                    say(format!("{} ... missed ({NOT_REACHED}) in 0.0s", names[&id]));
                } else {
                    line(id, status_of(ran).word(), took);
                }
            },
            &|r| {
                say(format!(
                    "{} mutant(s) did not end within {}s and are run again alone",
                    r.count,
                    timeout.as_secs()
                ));
            },
        )?;
        results.extend(ran.into_iter().map(|(id, ran, duration)| MutantResult {
            id,
            status: status_of(ran),
            duration,
            reason: (ran == Ran::Unreached).then(|| NOT_REACHED.to_owned()),
        }));
        // One at a time, each tested right after its build: the binaries of a
        // build (and the `CARGO_BIN_EXE_*` and working directory they carry)
        // are replaced by the next one. Nothing else runs now, so a timeout is final.
        for id in prepared.single_ids.clone() {
            interrupt::check()?;
            let t = Instant::now();
            let result = match prepared.builder.build(by_id[&id], false)? {
                Single::Unviable(why) => MutantResult {
                    id,
                    status: Status::Unviable,
                    duration: Duration::ZERO,
                    reason: Some(why),
                },
                Single::Built(bins) => {
                    let (ran, took) = tests.run(&bins, None, "0", Some(timeout))?;
                    MutantResult {
                        id,
                        status: status_of(ran),
                        duration: took,
                        reason: None,
                    }
                }
            };
            line(id, result.status.word(), t.elapsed());
            results.push(result);
        }
        say(format!(
            "mutants: run in {:.1}s",
            t_mutants.elapsed().as_secs_f64()
        ));
    }

    results.sort_by_key(|r| r.id);
    let counts = Counts::of(&results);
    let report = results::report_json(&mutants, &results, limit);
    results::write_output(out_dir, &mutants, &results, &report)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        );
    }
    if counts.missed > 0 {
        eprintln!("cargo schemata: {} surviving mutant(s)", counts.missed);
        for r in results.iter().filter(|r| r.status == Status::Missed) {
            match &r.reason {
                Some(why) => eprintln!("{} (missed: {why})", names[&r.id]),
                None => eprintln!("{} (missed)", names[&r.id]),
            }
        }
        eprintln!(
            "cargo schemata: FAILED ({})",
            counts.text(started.elapsed())
        );
        Ok(1)
    } else {
        say(format!(
            "cargo schemata: OK ({})",
            counts.text(started.elapsed())
        ));
        Ok(0)
    }
}

fn finish(result: Result<i32>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            if let Some(Interrupted(code)) = err.downcast_ref::<Interrupted>() {
                eprintln!("cargo schemata: interrupted; no report was written");
                return ExitCode::from(*code as u8);
            }
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse_from(strip_subcommand(std::env::args().collect()));
    if args.list || args.build_only {
        return match run_list_or_build(&args) {
            Ok(out) => {
                print!("{out}");
                ExitCode::SUCCESS
            }
            Err(err) => finish(Err(err)),
        };
    }
    if let Err(e) = interrupt::install() {
        eprintln!("error: cannot handle signals: {e}");
        return ExitCode::from(1);
    }
    finish(run_tests(&args))
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
        for opt in [
            "--in-diff=x",
            "--shard=1/2",
            "--html=x",
            "-j0",
            "--timeout=0",
            "--timeout=x",
        ] {
            assert!(
                Args::try_parse_from(v(&["x", "--list", opt])).is_err(),
                "{opt}"
            );
        }
    }

    #[test]
    fn parses_run_options() {
        let a = Args::try_parse_from(v(&[
            "x",
            "-j",
            "3",
            "--timeout",
            "2.5",
            "-o",
            "out",
            "--json",
        ]))
        .unwrap();
        assert_eq!(a.jobs, Some(3));
        assert_eq!(a.timeout, Some(2.5));
        assert_eq!(a.output, Utf8PathBuf::from("out"));
        assert!(a.json);
        let d = Args::try_parse_from(v(&["x"])).unwrap();
        assert_eq!(d.output, Utf8PathBuf::from("schemata.out"));
        assert_eq!((d.jobs, d.timeout), (None, None));
        assert!(Args::try_parse_from(v(&["x", "--list", "-j", "2"])).is_err());
    }

    #[test]
    fn splits_cargo_and_binary_arguments() {
        let t = split_test_args(&v(&["--lib", "--", "--test-threads", "1"]));
        assert_eq!(t.build, v(&["--lib"]));
        assert_eq!(t.binary, v(&["--test-threads", "1"]));
        // a bare word is a test name filter for the binaries, never for cargo check
        let t = split_test_args(&v(&[
            "--lib",
            "parse",
            "--features",
            "a,b",
            "-p",
            "x",
            "--",
            "--nocapture",
        ]));
        assert_eq!(t.build, v(&["--lib", "--features", "a,b", "-p", "x"]));
        assert_eq!(t.binary, v(&["parse", "--nocapture"]));
        // what `cargo check` does not take, or this program sets itself, is dropped
        let t = split_test_args(&v(&[
            "--no-fail-fast",
            "--test",
            "it",
            "--target-dir",
            "t",
            "--message-format=json",
            "--doc",
        ]));
        assert_eq!(t.build, v(&["--test", "it"]));
        assert_eq!(t.binary, Vec::<String>::new());
        assert_eq!(split_test_args(&[]), TestArgs::default());
        // only the first `--` splits
        let t = split_test_args(&v(&["--", "a", "--", "b"]));
        assert_eq!(t.binary, v(&["a", "--", "b"]));
    }
}

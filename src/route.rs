//! Route each mutant to the tests that can see it.
//!
//! The mutants are all in one build, so which tests reach a mutant's code does
//! not depend on the mutant: with `CARGO_SCHEMATA_TRACE` set, the runtime
//! records the ids it is asked for. The baseline run gives the ids any test
//! reaches. Then every test is run alone, and a mutant is run only against the
//! tests that reached it when alone (`<bin> --exact a b c`).
//!
//! Safeguards: a mutant that the whole run reaches but no single test does
//! (state set up by an earlier test) gets every test; a binary whose tests do
//! not all pass alone, or whose output is not libtest's, is always run whole;
//! so is a binary when most of its tests reach the mutant, or when the list of
//! names would be too long; tracing is given up when it takes too long.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::exec::{Step, Tests};
use crate::prepare::TestBinary;
use crate::proc::{Sink, run_group};

/// A mutant that most of a binary's tests reach gains nothing from a list.
const WHOLE_BINARY_PERCENT: usize = 80;
/// Longest total length of the test names put on one command line.
const MAX_NAMES_BYTES: usize = 32 * 1024;
/// The least time tracing may take, whatever the baseline.
const MIN_TRACE_BUDGET: Duration = Duration::from_secs(60);
/// How many times the baseline tracing may take.
const TRACE_BUDGET_MULTIPLIER: u32 = 10;

/// libtest options that take a value in the next argument.
const VALUE_OPTIONS: &[&str] = &[
    "--test-threads",
    "--format",
    "--color",
    "--logfile",
    "--shuffle-seed",
    "--report-time",
    "-Z",
];

/// The arguments of a test binary split into what selects tests (name
/// filters and `--skip`) and everything else.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LibtestArgs {
    pub selectors: Vec<String>,
    pub rest: Vec<String>,
}

pub fn split_libtest_args(args: &[String]) -> LibtestArgs {
    let mut out = LibtestArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--skip" {
            out.selectors.push(a.clone());
            out.selectors.extend(it.next().cloned());
        } else if a.starts_with("--skip=") || !a.starts_with('-') {
            out.selectors.push(a.clone());
        } else {
            out.rest.push(a.clone());
            if VALUE_OPTIONS.contains(&a.as_str()) {
                out.rest.extend(it.next().cloned());
            }
        }
    }
    out
}

/// Whether the arguments change libtest's output, or let a test write its own
/// lines into it, so that a `test <name> ... FAILED` line may not be libtest's:
/// then a run is not ended at such a line.
pub fn changes_output(args: &[String]) -> bool {
    args.iter().any(|a| {
        let name = a.split('=').next().unwrap_or(a);
        matches!(
            name,
            "--format"
                | "-q"
                | "--quiet"
                | "--color"
                | "--logfile"
                | "--list"
                | "-Z"
                | "--nocapture"
                | "--no-capture"
                | "--show-output"
        )
    })
}

/// The names in the output of `--list`.
pub fn parse_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

#[derive(Debug)]
struct BinRoute {
    /// Its tests were run alone and all passed; otherwise it always runs whole.
    traced: bool,
    tests: Vec<String>,
}

/// What to do for one mutant.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No test reaches it.
    Unreached,
    Steps(Vec<Step>),
}

#[derive(Debug)]
pub struct Routing {
    bins: Vec<BinRoute>,
    /// Arguments kept for every run of a listed test (not the selectors).
    rest: Vec<String>,
    /// Ids the whole baseline reached.
    global: HashSet<usize>,
    /// For each mutant, per binary, the indices of the tests that reached it alone.
    reach: HashMap<usize, Vec<Vec<usize>>>,
}

impl Routing {
    pub fn plan(&self, id: usize, nbins: usize) -> Plan {
        let whole = |bin: usize| Step { bin, args: None };
        if !self.global.contains(&id) {
            return Plan::Unreached;
        }
        let hits = self.reach.get(&id);
        let any = hits.is_some_and(|h| h.iter().any(|b| !b.is_empty()));
        if !any {
            return Plan::Steps((0..nbins).map(whole).collect());
        }
        let hits = hits.expect("any hit");
        let mut steps = Vec::new();
        for (bin, route) in self.bins.iter().enumerate() {
            if !route.traced {
                steps.push(whole(bin));
                continue;
            }
            let tests = &hits[bin];
            if tests.is_empty() {
                continue;
            }
            let bytes: usize = tests.iter().map(|&t| route.tests[t].len() + 1).sum();
            if tests.len() * 100 >= route.tests.len() * WHOLE_BINARY_PERCENT
                || bytes > MAX_NAMES_BYTES
            {
                steps.push(whole(bin));
                continue;
            }
            let mut args = self.rest.clone();
            if !args.iter().any(|a| a == "--exact") {
                args.push("--exact".to_owned());
            }
            args.extend(tests.iter().map(|&t| route.tests[t].clone()));
            steps.push(Step {
                bin,
                args: Some(args),
            });
        }
        Plan::Steps(steps)
    }
}

/// Read the ids in a trace file.
pub fn read_trace(path: &Path) -> HashSet<usize> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

pub struct Traced {
    pub routing: Routing,
    pub tests: usize,
    pub whole_binaries: usize,
}

pub enum TraceResult {
    Routing(Traced),
    /// It took longer than the budget; the elapsed time.
    GaveUp(Duration),
}

/// The most tracing may take: ten times the baseline, at least a minute.
pub fn trace_budget(baseline: Duration) -> Duration {
    (baseline * TRACE_BUDGET_MULTIPLIER).max(MIN_TRACE_BUDGET)
}

/// List the tests of each libtest binary and run each alone, `jobs` at a time,
/// recording the mutants it reaches.
pub fn trace(
    tests: &Tests,
    bins: &[TestBinary],
    libtest: &[bool],
    global: HashSet<usize>,
    jobs: usize,
    budget: Duration,
    timeout: Duration,
) -> Result<TraceResult> {
    let start = Instant::now();
    let split = split_libtest_args(tests.bin_args);
    let dir: PathBuf = tests.tmp_root.join("trace");
    std::fs::create_dir_all(&dir)?;
    let tmp = tests.tmp("list")?;

    let mut routes: Vec<BinRoute> = Vec::new();
    for (n, bin) in bins.iter().enumerate() {
        let mut traced = libtest.get(n).copied().unwrap_or(false);
        let mut names = Vec::new();
        if traced {
            let mut args = tests.bin_args.to_vec();
            args.push("--list".to_owned());
            let fin = run_group(
                &mut tests.command(bin, &args, None, None, &tmp),
                Some(timeout),
                &Sink::Capture,
            )?;
            match fin.status {
                Some(s) if s.success() => {
                    names = parse_list(&String::from_utf8_lossy(&fin.stdout));
                }
                _ => traced = false,
            }
        }
        routes.push(BinRoute {
            traced,
            tests: names,
        });
    }

    let work: Vec<(usize, usize)> = routes
        .iter()
        .enumerate()
        .filter(|(_, r)| r.traced)
        .flat_map(|(b, r)| (0..r.tests.len()).map(move |t| (b, t)))
        .collect();
    let broken: Vec<AtomicBool> = routes.iter().map(|_| AtomicBool::new(false)).collect();
    let over = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    let error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let file = |b: usize, t: usize| dir.join(format!("{b}-{t}.txt"));
    std::thread::scope(|scope| {
        for worker in 0..jobs.min(work.len()).max(1) {
            let (routes, broken, over, next, error, work, file) =
                (&routes, &broken, &over, &next, &error, &work, &file);
            let rest = &split.rest;
            scope.spawn(move || {
                let job = format!("trace-{worker}");
                loop {
                    if over.load(Ordering::Relaxed) {
                        break;
                    }
                    if start.elapsed() > budget {
                        over.store(true, Ordering::Relaxed);
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(b, t)) = work.get(i) else { break };
                    let one = || -> Result<bool> {
                        let tmp = tests.tmp(&job)?;
                        let mut args = rest.clone();
                        if !args.iter().any(|a| a == "--exact") {
                            args.push("--exact".to_owned());
                        }
                        args.push(routes[b].tests[t].clone());
                        let path = file(b, t);
                        let _ = std::fs::remove_file(&path);
                        let fin = run_group(
                            &mut tests.command(&bins[b], &args, None, Some(&path), &tmp),
                            Some(timeout),
                            &Sink::Null,
                        )?;
                        Ok(fin.status.is_some_and(|s| s.success()))
                    };
                    match one() {
                        Ok(true) => {}
                        Ok(false) => broken[b].store(true, Ordering::Relaxed),
                        Err(e) => {
                            error.lock().expect("error").get_or_insert(e);
                            over.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().expect("error") {
        return Err(e);
    }
    if start.elapsed() > budget || over.load(Ordering::Relaxed) {
        return Ok(TraceResult::GaveUp(start.elapsed()));
    }

    let mut reach: HashMap<usize, Vec<Vec<usize>>> = HashMap::new();
    let mut count = 0;
    for (b, route) in routes.iter_mut().enumerate() {
        if broken[b].load(Ordering::Relaxed) {
            route.traced = false;
        }
        if !route.traced {
            continue;
        }
        count += route.tests.len();
        for t in 0..route.tests.len() {
            for id in read_trace(&file(b, t)) {
                reach
                    .entry(id)
                    .or_insert_with(|| vec![Vec::new(); bins.len()])[b]
                    .push(t);
            }
        }
    }
    let whole_binaries = routes.iter().filter(|r| !r.traced).count();
    Ok(TraceResult::Routing(Traced {
        routing: Routing {
            bins: routes,
            rest: split.rest,
            global,
            reach,
        },
        tests: count,
        whole_binaries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn selectors_are_split_from_the_other_arguments() {
        let s = split_libtest_args(&v(&[
            "alpha",
            "--skip",
            "slow",
            "--test-threads",
            "2",
            "--skip=other",
            "--nocapture",
            "beta",
        ]));
        assert_eq!(
            s.selectors,
            v(&["alpha", "--skip", "slow", "--skip=other", "beta"])
        );
        assert_eq!(s.rest, v(&["--test-threads", "2", "--nocapture"]));
        assert_eq!(split_libtest_args(&[]), LibtestArgs::default());
    }

    #[test]
    fn output_changing_arguments_are_found() {
        assert!(changes_output(&v(&["--format", "terse"])));
        assert!(changes_output(&v(&["--format=json"])));
        assert!(changes_output(&v(&["-q"])));
        assert!(changes_output(&v(&["--nocapture"])));
        assert!(changes_output(&v(&["--show-output"])));
        assert!(!changes_output(&v(&["--test-threads=1", "--ignored", "x"])));
    }

    #[test]
    fn the_list_has_tests_only() {
        let out = "a::b: test\nc: benchmark\nd: test\n\n2 tests, 1 benchmark\n";
        assert_eq!(parse_list(out), v(&["a::b", "d"]));
    }

    #[test]
    fn the_budget_is_ten_baselines_but_at_least_a_minute() {
        assert_eq!(
            trace_budget(Duration::from_secs(1)),
            Duration::from_secs(60)
        );
        assert_eq!(
            trace_budget(Duration::from_secs(8)),
            Duration::from_secs(80)
        );
    }

    fn routing(traced: bool, tests: usize) -> Routing {
        Routing {
            bins: vec![BinRoute {
                traced,
                tests: (0..tests).map(|i| format!("t{i}")).collect(),
            }],
            rest: v(&["--test-threads", "2"]),
            global: [1, 2, 3].into(),
            reach: HashMap::from([(1, vec![vec![0, 1]]), (2, vec![vec![]])]),
        }
    }

    #[test]
    fn a_mutant_gets_the_tests_that_reached_it() {
        let r = routing(true, 10);
        assert_eq!(
            r.plan(1, 1),
            Plan::Steps(vec![Step {
                bin: 0,
                args: Some(v(&["--test-threads", "2", "--exact", "t0", "t1"]))
            }])
        );
    }

    #[test]
    fn nothing_reaches_it_or_only_the_whole_run_does() {
        let r = routing(true, 10);
        assert_eq!(r.plan(9, 1), Plan::Unreached);
        let whole = Plan::Steps(vec![Step { bin: 0, args: None }]);
        assert_eq!(r.plan(2, 1), whole);
        assert_eq!(r.plan(3, 1), whole);
    }

    #[test]
    fn a_binary_that_is_not_traced_or_mostly_reached_runs_whole() {
        let whole = Plan::Steps(vec![Step { bin: 0, args: None }]);
        assert_eq!(routing(false, 10).plan(1, 1), whole);
        // 2 of 2 tests
        assert_eq!(routing(true, 2).plan(1, 1), whole);
        // 2 of 4 is below 80%
        assert_ne!(routing(true, 4).plan(1, 1), whole);
    }
}

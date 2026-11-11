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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BinRoute {
    /// Its tests were run alone and all passed; otherwise it always runs whole.
    traced: bool,
    tests: Vec<String>,
    /// What each test took when it ran alone, process start included.
    times: Vec<Duration>,
    /// What the whole binary took in the baseline.
    whole: Duration,
}

/// What to do for one mutant.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No test reaches it.
    Unreached,
    Steps(Vec<Step>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    bins: Vec<BinRoute>,
    /// Arguments kept for every run of a listed test (not the selectors).
    rest: Vec<String>,
    /// Ids the whole baseline reached.
    global: BTreeSet<usize>,
    /// For each mutant, per binary, the indices of the tests that reached it alone.
    reach: BTreeMap<usize, Vec<Vec<usize>>>,
}

impl Routing {
    /// How many tests were run alone, and how many binaries always run whole.
    pub fn summary(&self) -> (usize, usize) {
        let alone = self.bins.iter().filter(|b| b.traced);
        (
            alone.map(|b| b.tests.len()).sum(),
            self.bins.iter().filter(|b| !b.traced).count(),
        )
    }

    /// Whether a routing that was read from a file fits `nbins` test binaries:
    /// every index it holds has to exist, so that a plan cannot be made from
    /// what is not there.
    pub fn check(&self, nbins: usize) -> Result<()> {
        ensure!(
            self.bins.len() == nbins,
            "it has {} test binaries, not {nbins}",
            self.bins.len()
        );
        for b in &self.bins {
            ensure!(
                b.times.len() == b.tests.len(),
                "a binary has {} tests and {} times",
                b.tests.len(),
                b.times.len()
            );
        }
        for (id, hits) in &self.reach {
            ensure!(
                hits.len() == nbins,
                "mutant {id} has {} binaries, not {nbins}",
                hits.len()
            );
            for (b, tests) in hits.iter().enumerate() {
                ensure!(
                    tests.iter().all(|&t| t < self.bins[b].tests.len()),
                    "mutant {id} names a test that binary {b} does not have"
                );
            }
        }
        Ok(())
    }

    /// Make a routing that was traced on a build of `traced` (every mutant of
    /// the run) the routing a build of `here` (the mutants of one part) would
    /// have traced itself, or `None` when that part would have had no routing.
    ///
    /// A build records only its own mutants, so the others are dropped. When
    /// the baseline then reached none of them, a part that traced itself
    /// turns routing off and runs every test; so does this. A mutant that is
    /// in this build and was not in the traced one (it did not compile there,
    /// or was built on its own) was never recorded: it is not "unreached", it
    /// gets every test.
    pub fn for_build(mut self, here: &BTreeSet<usize>, traced: &BTreeSet<usize>) -> Option<Self> {
        self.global.retain(|id| here.contains(id));
        self.reach.retain(|id, _| here.contains(id));
        if self.global.is_empty() {
            return None;
        }
        for id in here.difference(traced) {
            self.global.insert(*id);
            self.reach.remove(id);
        }
        Some(self)
    }

    /// The same routing for the binaries in another order: binary `b` of this
    /// routing is binary `to[b]` of the result. `to` is a permutation.
    pub fn reordered(self, to: &[usize]) -> Self {
        let place = |old: Vec<Vec<usize>>| {
            let mut new = vec![Vec::new(); old.len()];
            for (b, tests) in old.into_iter().enumerate() {
                new[to[b]] = tests;
            }
            new
        };
        let mut bins: Vec<Option<BinRoute>> = self.bins.iter().map(|_| None).collect();
        for (b, route) in self.bins.into_iter().enumerate() {
            bins[to[b]] = Some(route);
        }
        Routing {
            bins: bins
                .into_iter()
                .map(|b| b.expect("a permutation"))
                .collect(),
            rest: self.rest,
            global: self.global,
            reach: self
                .reach
                .into_iter()
                .map(|(id, h)| (id, place(h)))
                .collect(),
        }
    }

    /// A routing of `nbins` binaries that differ from each other, for tests.
    #[cfg(test)]
    pub fn sample(nbins: usize) -> Routing {
        Routing {
            bins: (0..nbins)
                .map(|b| BinRoute {
                    traced: b != 1,
                    tests: (0..10).map(|t| format!("b{b}::t{t}")).collect(),
                    times: (0..10)
                        .map(|t| Duration::from_millis(10 * (b as u64 + 1) + t))
                        .collect(),
                    whole: Duration::from_secs(b as u64 + 1),
                })
                .collect(),
            rest: vec!["--test-threads".to_owned(), "1".to_owned()],
            global: [1, 2, 3].into(),
            reach: BTreeMap::from([
                (1, (0..nbins).map(|b| vec![b, b + 1]).collect()),
                (2, (0..nbins).map(|b| vec![9 - b]).collect()),
            ]),
        }
    }

    #[cfg(test)]
    fn plan(&self, id: usize, nbins: usize) -> Plan {
        self.plan_and_estimate(id, nbins).0
    }

    /// The plan, and what its steps took without a mutant: the baseline time
    /// of a binary that runs whole, and for listed tests the sum of what each
    /// took alone (more than they take together in one process). `None` when
    /// every binary runs whole because no single test reaches the mutant: the
    /// time of the whole baseline is the estimate then.
    pub fn plan_and_estimate(&self, id: usize, nbins: usize) -> (Plan, Option<Duration>) {
        let whole = |bin: usize| Step { bin, args: None };
        if !self.global.contains(&id) {
            return (Plan::Unreached, None);
        }
        let hits = self.reach.get(&id);
        let any = hits.is_some_and(|h| h.iter().any(|b| !b.is_empty()));
        if !any {
            let mut steps: Vec<Step> = (0..nbins).map(whole).collect();
            steps.sort_by_key(|s| self.bins.get(s.bin).map_or(Duration::MAX, |r| r.whole));
            return (Plan::Steps(steps), None);
        }
        let hits = hits.expect("any hit");
        // Each step with what it took without a mutant.
        let mut steps: Vec<(Duration, Step)> = Vec::new();
        for (bin, route) in self.bins.iter().enumerate() {
            if !route.traced {
                steps.push((route.whole, whole(bin)));
                continue;
            }
            let mut tests = hits[bin].clone();
            if tests.is_empty() {
                continue;
            }
            let bytes: usize = tests.iter().map(|&t| route.tests[t].len() + 1).sum();
            if tests.len() * 100 >= route.tests.len() * WHOLE_BINARY_PERCENT
                || bytes > MAX_NAMES_BYTES
            {
                steps.push((route.whole, whole(bin)));
                continue;
            }
            // The quickest first here too. libtest runs the tests it is given
            // in the order of their names, not of the arguments, so this does
            // not change which test of a binary runs first.
            tests.sort_by_key(|&t| (route.times[t], t));
            let took = tests.iter().map(|&t| route.times[t]).sum::<Duration>();
            let mut args = self.rest.clone();
            if !args.iter().any(|a| a == "--exact") {
                args.push("--exact".to_owned());
            }
            args.extend(tests.iter().map(|&t| route.tests[t].clone()));
            steps.push((
                took,
                Step {
                    bin,
                    args: Some(args),
                },
            ));
        }
        let estimate = steps.iter().map(|s| s.0).sum();
        // The quickest step first: the first step that fails catches the mutant
        // and the later ones are not started. The sort is stable, so steps that
        // took the same stay in the order of the binaries.
        steps.sort_by_key(|s| s.0);
        let steps = steps.into_iter().map(|s| s.1).collect();
        (Plan::Steps(steps), Some(estimate))
    }
}

/// Read the ids in a trace file.
pub fn read_trace(path: &Path) -> BTreeSet<usize> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

pub enum TraceResult {
    Routing(Routing),
    /// It took longer than the budget; the elapsed time.
    GaveUp(Duration),
}

/// The most tracing may take: ten times the baseline, at least a minute.
pub fn trace_budget(baseline: Duration) -> Duration {
    (baseline * TRACE_BUDGET_MULTIPLIER).max(MIN_TRACE_BUDGET)
}

/// What the baseline found out about the binaries, one entry per binary.
pub struct BaselineRun<'a> {
    /// Its output is libtest's.
    pub libtest: &'a [bool],
    /// What it took.
    pub took: &'a [Duration],
}

/// List the tests of each libtest binary and run each alone, `jobs` at a time,
/// recording the mutants it reaches and the time it takes.
pub fn trace(
    tests: &Tests,
    bins: &[TestBinary],
    baseline: &BaselineRun,
    global: BTreeSet<usize>,
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
        let mut traced = baseline.libtest.get(n).copied().unwrap_or(false);
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
            times: vec![Duration::ZERO; names.len()],
            tests: names,
            whole: baseline.took.get(n).copied().unwrap_or_default(),
        });
    }

    let work: Vec<(usize, usize)> = routes
        .iter()
        .enumerate()
        .filter(|(_, r)| r.traced)
        .flat_map(|(b, r)| (0..r.tests.len()).map(move |t| (b, t)))
        .collect();
    let broken: Vec<AtomicBool> = routes.iter().map(|_| AtomicBool::new(false)).collect();
    // Microseconds each run took, in the order of `work`.
    let micros: Vec<AtomicU64> = work.iter().map(|_| AtomicU64::new(0)).collect();
    let over = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    let error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let file = |b: usize, t: usize| dir.join(format!("{b}-{t}.txt"));
    std::thread::scope(|scope| {
        for worker in 0..jobs.min(work.len()).max(1) {
            let (routes, broken, over, next, error, work, file, micros) = (
                &routes, &broken, &over, &next, &error, &work, &file, &micros,
            );
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
                        let began = Instant::now();
                        let fin = run_group(
                            &mut tests.command(&bins[b], &args, None, Some(&path), &tmp),
                            Some(timeout),
                            &Sink::Null,
                        )?;
                        let took = u64::try_from(began.elapsed().as_micros()).unwrap_or(u64::MAX);
                        micros[i].store(took, Ordering::Relaxed);
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

    for (&(b, t), took) in work.iter().zip(&micros) {
        routes[b].times[t] = Duration::from_micros(took.load(Ordering::Relaxed));
    }
    let mut reach: BTreeMap<usize, Vec<Vec<usize>>> = BTreeMap::new();
    for (b, route) in routes.iter_mut().enumerate() {
        if broken[b].load(Ordering::Relaxed) {
            route.traced = false;
        }
        if !route.traced {
            continue;
        }
        for t in 0..route.tests.len() {
            for id in read_trace(&file(b, t)) {
                reach
                    .entry(id)
                    .or_insert_with(|| vec![Vec::new(); bins.len()])[b]
                    .push(t);
            }
        }
    }
    Ok(TraceResult::Routing(Routing {
        bins: routes,
        rest: split.rest,
        global,
        reach,
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

    impl Routing {
        fn times_mut(&mut self) -> &mut Vec<Duration> {
            &mut self.bins[0].times
        }
    }

    fn routing(traced: bool, tests: usize) -> Routing {
        Routing {
            bins: vec![BinRoute {
                traced,
                tests: (0..tests).map(|i| format!("t{i}")).collect(),
                // t0 took 100 ms alone, t1 200 ms, ...
                times: (1..=tests as u64)
                    .map(|i| Duration::from_millis(i * 100))
                    .collect(),
                whole: Duration::from_secs(7),
            }],
            rest: v(&["--test-threads", "2"]),
            global: [1, 2, 3].into(),
            reach: BTreeMap::from([(1, vec![vec![0, 1]]), (2, vec![vec![]])]),
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

    #[test]
    fn the_estimate_is_the_time_of_the_steps_of_the_plan() {
        let est = |r: &Routing, id| r.plan_and_estimate(id, 1).1;
        // the tests t0 and t1, each run alone: 100 ms + 200 ms
        assert_eq!(est(&routing(true, 10), 1), Some(Duration::from_millis(300)));
        // a binary that runs whole (not traced, or mostly reached): its baseline
        assert_eq!(est(&routing(false, 10), 1), Some(Duration::from_secs(7)));
        assert_eq!(est(&routing(true, 2), 1), Some(Duration::from_secs(7)));
        // no single test reaches it, so everything runs: the whole baseline
        assert_eq!(est(&routing(true, 10), 2), None);
        assert_eq!(est(&routing(true, 10), 3), None);
        // not reached at all: nothing runs
        assert_eq!(
            routing(true, 10).plan_and_estimate(9, 1),
            (Plan::Unreached, None)
        );
        // the plan is the same one
        for id in [1, 2, 3, 9] {
            let r = routing(true, 10);
            assert_eq!(r.plan_and_estimate(id, 1).0, r.plan(id, 1));
        }
    }

    #[test]
    fn only_the_first_run_of_several_jobs_is_given_up_sooner() {
        use crate::exec::{Pass, Pool};
        let secs = Duration::from_secs;
        let mut r = routing(true, 10);
        // t9 takes 4 s alone, and only it reaches mutant 4
        r.times_mut()[9] = secs(4);
        r.global.insert(4);
        r.reach.insert(4, vec![vec![9]]);
        let pool = Pool {
            jobs: 2,
            timeout: secs(120),
            routed_floor: Some(secs(10)),
            routing: Some(&r),
        };
        let limit = |pool: &Pool, pass, id| {
            let estimate = pool.routing.and_then(|r| r.plan_and_estimate(id, 1).1);
            pool.limit(pass, estimate)
        };
        // 300 ms of tests: the floor. 4 s of tests: five times.
        assert_eq!(limit(&pool, Pass::First, 1), secs(10));
        assert_eq!(limit(&pool, Pass::First, 4), secs(20));
        // every test runs (no single test reaches it): the whole limit
        assert_eq!(limit(&pool, Pass::First, 2), secs(120));
        // the run that decides always gets the whole limit: the run alone ...
        for id in [1, 2, 4] {
            assert_eq!(limit(&pool, Pass::Alone, id), secs(120));
        }
        // ... and the only run of a single job
        let single = Pool { jobs: 1, ..pool };
        for id in [1, 2, 4] {
            assert_eq!(limit(&single, Pass::First, id), secs(120));
        }
        // an explicit limit is the limit of every run
        let given = Pool {
            timeout: secs(3),
            routed_floor: None,
            ..pool
        };
        for (pass, id) in [(Pass::First, 1), (Pass::First, 4), (Pass::Alone, 1)] {
            assert_eq!(limit(&given, pass, id), secs(3));
        }
        // and so is the whole limit without routing
        let unrouted = Pool {
            routing: None,
            ..pool
        };
        assert_eq!(limit(&unrouted, Pass::First, 1), secs(120));
        // a binary that runs whole: five times its baseline of 7 s
        let whole = routing(false, 10);
        let pool = Pool {
            routing: Some(&whole),
            ..pool
        };
        assert_eq!(limit(&pool, Pass::First, 1), secs(35));
    }

    /// The limit made from a mutant's own tests never decides its status: a
    /// mutant that is slower than that limit but ends is caught or missed, as
    /// it is with the limit of the whole suite; only one that does not end
    /// within the whole limit is a time-out.
    #[test]
    fn a_mutant_slower_than_the_limit_of_its_own_tests_is_not_a_timeout() {
        use crate::exec::{Pool, Ran, run_embedded};
        let dir = std::env::temp_dir().join(format!("schemata-route-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 1 survives and 3 is caught, both after 0.7 s; 2 never ends; 4 is quick
        let script = "case \"$CARGO_SCHEMATA_MUTANT\" in 1) sleep 0.7; exit 0;; 2) sleep 30;; 3) sleep 0.7; exit 1;; *) exit 0;; esac";
        let bin = TestBinary {
            path: "/bin/sh".into(),
            cwd: "/".into(),
            env: Vec::new(),
            package: "p".into(),
            target: "t".into(),
            kind: "lib".into(),
        };
        let bin_args = v(&["-c", script]);
        let tests = Tests {
            bin_args: &bin_args,
            tmp_root: &dir,
            early_kill: false,
        };
        // every mutant is reached by one test that took no time alone, so the
        // first run gets the floor
        let routing = Routing {
            bins: vec![BinRoute {
                traced: true,
                tests: (0..10).map(|i| format!("t{i}")).collect(),
                times: vec![Duration::ZERO; 10],
                whole: Duration::from_millis(100),
            }],
            rest: bin_args.clone(),
            global: [1, 2, 3, 4].into(),
            reach: (1..=4).map(|id| (id, vec![vec![id]])).collect(),
        };
        let run = |jobs: usize, ids: &[usize]| {
            let reruns = AtomicUsize::new(0);
            let out = run_embedded(
                &tests,
                std::slice::from_ref(&bin),
                ids,
                &Pool {
                    jobs,
                    timeout: Duration::from_secs(2),
                    routed_floor: Some(Duration::from_millis(200)),
                    routing: Some(&routing),
                },
                &|_, _, _| {},
                &|r| reruns.store(r.count, Ordering::Relaxed),
            )
            .unwrap();
            (out, reruns.load(Ordering::Relaxed))
        };

        let (out, reruns) = run(2, &[1, 2, 3, 4]);
        let statuses: Vec<(usize, Ran)> = out.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            statuses,
            [
                (1, Ran::Missed),
                (2, Ran::Timeout),
                (3, Ran::Caught),
                (4, Ran::Missed)
            ]
        );
        // the three slow ones reached 200 ms and were run again alone
        assert_eq!(reruns, 3);
        // the time is the one of the run alone: the hang got the whole 2 s
        assert!(out[0].2 >= Duration::from_millis(700), "{:?}", out[0].2);
        assert!(out[1].2 >= Duration::from_secs(2), "{:?}", out[1].2);

        // a single job has no second run, so its only run gets the whole limit
        let (out, reruns) = run(1, &[1, 3]);
        let statuses: Vec<(usize, Ran)> = out.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(statuses, [(1, Ran::Missed), (3, Ran::Caught)]);
        assert_eq!(reruns, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_binaries_add_up() {
        let mut r = routing(true, 10);
        r.bins.push(BinRoute {
            traced: false,
            tests: Vec::new(),
            times: Vec::new(),
            whole: Duration::from_secs(2),
        });
        r.reach = BTreeMap::from([(1, vec![vec![2], vec![]])]);
        let (plan, estimate) = r.plan_and_estimate(1, 2);
        // t2 alone (300 ms) and the second binary whole (2 s)
        assert_eq!(estimate, Some(Duration::from_millis(2300)));
        let Plan::Steps(steps) = plan else {
            panic!("steps")
        };
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1], Step { bin: 1, args: None });
    }

    #[test]
    fn the_quickest_step_and_the_quickest_test_come_first() {
        let ms = Duration::from_millis;
        let bin = |traced: bool, times: &[u64], whole: u64| BinRoute {
            traced,
            tests: (0..times.len()).map(|i| format!("t{i}")).collect(),
            times: times.iter().map(|&t| ms(t)).collect(),
            whole: ms(whole),
        };
        let r = Routing {
            bins: vec![
                // 0: two slow tests reach the mutant
                bin(true, &[900, 10, 10, 10, 600, 10, 10, 10, 10, 10], 2000),
                // 1: not traced, runs whole in 700 ms
                bin(false, &[], 700),
                // 2: nothing reaches the mutant
                bin(true, &[5, 5, 5], 20),
                // 3: one quick test and two that took the same
                bin(true, &[40, 30, 10, 10, 30, 10, 10, 10, 10, 10], 300),
                // 4: runs whole (2 of 2 tests), as long as binary 1
                bin(true, &[1, 1], 700),
            ],
            rest: v(&["--test-threads", "2"]),
            global: [1, 2].into(),
            reach: BTreeMap::from([(
                1,
                vec![vec![0, 4], vec![], vec![], vec![0, 1, 4], vec![0, 1]],
            )]),
        };
        let listed = |bin, names: &[&str]| {
            let mut args = v(&["--test-threads", "2", "--exact"]);
            args.extend(v(names));
            Step {
                bin,
                args: Some(args),
            }
        };
        let whole = |bin| Step { bin, args: None };
        let (plan, estimate) = r.plan_and_estimate(1, 5);
        assert_eq!(
            plan,
            Plan::Steps(vec![
                // 100 ms; t1 and t4 took the same and keep their order
                listed(3, &["t1", "t4", "t0"]),
                // 700 ms each: the order of the binaries
                whole(1),
                whole(4),
                // 1500 ms
                listed(0, &["t4", "t0"]),
            ])
        );
        // the estimate does not depend on the order
        assert_eq!(estimate, Some(ms(100 + 700 + 700 + 1500)));

        // no single test reaches the mutant: every binary whole, the quickest first
        let (plan, estimate) = r.plan_and_estimate(2, 5);
        assert_eq!(
            plan,
            Plan::Steps(vec![whole(2), whole(3), whole(1), whole(4), whole(0)])
        );
        assert_eq!(estimate, None);
    }

    #[test]
    fn a_routing_read_back_from_json_is_the_same_routing() {
        let mut r = routing(true, 10);
        r.bins[0].times[3] = Duration::new(2, 123_456_789);
        let text = serde_json::to_string(&r).unwrap();
        let back: Routing = serde_json::from_str(&text).unwrap();
        assert_eq!(back, r);
        // the same text again: nothing in it depends on the order of a hash
        assert_eq!(serde_json::to_string(&back).unwrap(), text);
        for id in 0..5 {
            assert_eq!(back.plan_and_estimate(id, 1), r.plan_and_estimate(id, 1));
        }
    }

    #[test]
    fn a_routing_for_a_part_is_what_the_part_would_have_traced() {
        // traced on a build of 1, 2 and 3; the baseline reached all three
        let traced: BTreeSet<usize> = [1, 2, 3].into();
        // a part with 1 and 3: mutant 2 is dropped, the plans of 1 and 3 stay
        let whole = routing(true, 10);
        let part = routing(true, 10)
            .for_build(&[1, 3].into(), &traced)
            .unwrap();
        assert_eq!(part.global, [1, 3].into());
        assert_eq!(part.reach.keys().copied().collect::<Vec<_>>(), vec![1]);
        for id in [1, 3] {
            assert_eq!(
                part.plan_and_estimate(id, 1),
                whole.plan_and_estimate(id, 1)
            );
        }
        // the mutants of another part are not planned here
        assert_eq!(part.plan(2, 1), Plan::Unreached);

        // a part none of whose mutants the baseline reached has no routing,
        // as when it traces itself ("the baseline reached no mutant")
        assert_eq!(routing(true, 10).for_build(&[7, 8].into(), &traced), None);
        let all: BTreeSet<usize> = [1, 2, 3, 7, 8].into();
        assert_eq!(routing(true, 10).for_build(&[7, 8].into(), &all), None);

        // 9 is built here but was not in the traced build: never recorded, so
        // it is not unreached. It gets every test.
        let part = routing(true, 10)
            .for_build(&[1, 9].into(), &traced)
            .unwrap();
        assert_eq!(
            part.plan_and_estimate(9, 1),
            (Plan::Steps(vec![Step { bin: 0, args: None }]), None)
        );
        // while 7, which was in the traced build and was not reached, is unreached
        let part = routing(true, 10).for_build(&[1, 7].into(), &all).unwrap();
        assert_eq!(part.plan(7, 1), Plan::Unreached);
    }

    #[test]
    fn a_routing_follows_the_binaries_to_another_order() {
        let r = Routing::sample(3);
        // binary 0 is now the last one, 1 the first, 2 the second
        let moved = r.clone().reordered(&[2, 0, 1]);
        assert!(moved.check(3).is_ok());
        assert_eq!(moved.bins[2], r.bins[0]);
        assert_eq!(moved.bins[0], r.bins[1]);
        assert_eq!(moved.bins[1], r.bins[2]);
        for id in [1, 2] {
            assert_eq!(moved.reach[&id][2], r.reach[&id][0]);
            assert_eq!(moved.reach[&id][0], r.reach[&id][1]);
            assert_eq!(moved.reach[&id][1], r.reach[&id][2]);
        }
        // the same steps, with the binaries under their new numbers
        let renumber = |plan: Plan| match plan {
            Plan::Steps(steps) => Plan::Steps(
                steps
                    .into_iter()
                    .map(|s| Step {
                        bin: [2, 0, 1][s.bin],
                        args: s.args,
                    })
                    .collect(),
            ),
            Plan::Unreached => Plan::Unreached,
        };
        for id in 0..5 {
            let (plan, estimate) = r.plan_and_estimate(id, 3);
            let (moved_plan, moved_estimate) = moved.plan_and_estimate(id, 3);
            assert_eq!(moved_estimate, estimate, "{id}");
            // the order of steps that took the same may follow the binaries
            let sorted = |p: Plan| match p {
                Plan::Steps(mut s) => {
                    s.sort_by_key(|s| s.bin);
                    Plan::Steps(s)
                }
                Plan::Unreached => Plan::Unreached,
            };
            assert_eq!(sorted(moved_plan), sorted(renumber(plan)), "{id}");
        }
        assert_eq!(moved.reordered(&[1, 2, 0]), r);
    }

    #[test]
    fn a_routing_that_does_not_fit_the_binaries_is_refused() {
        assert!(routing(true, 10).check(1).is_ok());
        assert!(routing(true, 10).check(2).is_err());
        let mut r = routing(true, 10);
        r.reach.insert(3, vec![vec![10]]);
        assert!(r.check(1).is_err());
        let mut r = routing(true, 10);
        r.reach.insert(3, vec![vec![], vec![]]);
        assert!(r.check(1).is_err());
        let mut r = routing(true, 10);
        r.bins[0].times.pop();
        assert!(r.check(1).is_err());
    }
}

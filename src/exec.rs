//! Run the test binaries: once without a mutant (the baseline), then once per
//! mutant, several at a time.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::interrupt;
use crate::prepare::{MUTANT_ENV, TestBinary};
use crate::proc::{Sink, run_group};
use crate::rewrite::TRACE_ENV;
use crate::route::{Plan, Routing};

/// The least time a mutant gets, as in cargo-mutants 27.1.0.
const MINIMUM_TIMEOUT: Duration = Duration::from_secs(20);
/// What the baseline is multiplied by, as in cargo-mutants 27.1.0.
const TIMEOUT_MULTIPLIER: f64 = 5.0;

// Copyright 2023-2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.
/// The time the tests of one mutant get: the explicit limit, or the baseline
/// times five, at least 20 seconds (cargo-mutants 27.1.0, `src/timeouts.rs`).
pub fn mutant_timeout(baseline: Duration, explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or_else(|| {
        MINIMUM_TIMEOUT.max(Duration::from_secs_f64(
            (baseline.as_secs_f64() * TIMEOUT_MULTIPLIER).ceil(),
        ))
    })
}

/// The least time a routed mutant gets in the first pass of several jobs.
pub const MINIMUM_ROUTED_TIMEOUT: Duration = Duration::from_secs(10);

/// The time a mutant that runs only some of the tests gets in the first pass of
/// several jobs: five times what those tests took without a mutant
/// (`estimate`), rounded up to the second and at least `floor`, but never more
/// than `whole`, the limit of a mutant that runs every test ([`mutant_timeout`]).
///
/// It only decides when a job is given to the next mutant, never the result: a
/// mutant that reaches it is run again alone with `whole`. A mutant in an
/// endless loop then holds a job for seconds, not for five times the whole
/// test suite.
pub fn routed_timeout(estimate: Duration, floor: Duration, whole: Duration) -> Duration {
    let own = floor.max(Duration::from_secs_f64(
        (estimate.as_secs_f64() * TIMEOUT_MULTIPLIER).ceil(),
    ));
    own.min(whole)
}

/// A quarter of the processors, at least 1 and at most 4.
pub fn default_jobs() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    (cpus / 4).clamp(1, 4)
}

/// How the test binaries are started.
pub struct Tests<'a> {
    /// Arguments for every test binary (name filters, `--test-threads`).
    pub bin_args: &'a [String],
    /// Each job gets a directory of its own below this, as `TMPDIR`.
    pub tmp_root: &'a Path,
    /// End a run as soon as libtest reports a failed test.
    pub early_kill: bool,
}

/// One test binary to start for a mutant, and with which arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Index into the binaries.
    pub bin: usize,
    /// Replaces [`Tests::bin_args`] when set.
    pub args: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    Caught,
    Missed,
    Timeout,
    /// Not run: no test reaches the mutant's code. Counts as missed.
    Unreached,
}

impl Tests<'_> {
    pub fn command(
        &self,
        bin: &TestBinary,
        args: &[String],
        mutant: Option<usize>,
        trace: Option<&Path>,
        tmp: &Path,
    ) -> Command {
        let mut cmd = Command::new(&bin.path);
        cmd.args(args)
            .current_dir(&bin.cwd)
            .env_remove(MUTANT_ENV)
            .env_remove(TRACE_ENV)
            .envs(bin.env.iter().map(|(k, v)| (k, v)))
            .env("INSTA_UPDATE", "no")
            .env("TMPDIR", tmp);
        if let Some(id) = mutant {
            cmd.env(MUTANT_ENV, id.to_string());
        }
        if let Some(file) = trace {
            cmd.env(TRACE_ENV, file);
        }
        cmd
    }

    pub fn tmp(&self, job: &str) -> Result<PathBuf> {
        let tmp = self.tmp_root.join(job);
        // A fresh directory for every run: leftovers from an earlier mutant
        // (or the baseline) must not reach this one.
        crate::tmpdir::force_remove_dir_all(&tmp)
            .with_context(|| format!("clear {}", tmp.display()))?;
        std::fs::create_dir_all(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        Ok(tmp)
    }

    /// Run the binaries one after the other, for `mutant` (or for none). The
    /// first to fail catches it and the rest are not started; the limit covers
    /// all of them together.
    pub fn run(
        &self,
        bins: &[TestBinary],
        mutant: Option<usize>,
        job: &str,
        timeout: Option<Duration>,
    ) -> Result<(Ran, Duration)> {
        let steps: Vec<Step> = (0..bins.len())
            .map(|bin| Step { bin, args: None })
            .collect();
        self.run_steps(bins, &steps, mutant, job, timeout, false)
    }

    /// Like [`Tests::run`], for these steps, in the order given.
    ///
    /// The steps of a routed mutant are ordered by what they took, not by
    /// binary. When one of them does not end within the limit, a binary that
    /// comes before it in the order of the binaries may not have run yet; in
    /// that order it would have run first and could have caught the mutant.
    /// And the step itself, when it was not the first, had only what the steps
    /// before it left of the limit. With `decides` (the result of this run is
    /// final) those steps, and then that step, are run with the limit anew:
    /// caught if one fails, a time-out otherwise.
    pub fn run_steps(
        &self,
        bins: &[TestBinary],
        steps: &[Step],
        mutant: Option<usize>,
        job: &str,
        timeout: Option<Duration>,
        decides: bool,
    ) -> Result<(Ran, Duration)> {
        let tmp = self.tmp(job)?;
        let start = Instant::now();
        for (n, step) in steps.iter().enumerate() {
            let left = timeout.map(|t| t.saturating_sub(start.elapsed()));
            match self.run_step(bins, step, mutant, &tmp, left)? {
                Ran::Missed => {}
                Ran::Timeout if decides => {
                    let mut earlier: Vec<&Step> =
                        steps[n + 1..].iter().filter(|s| s.bin < step.bin).collect();
                    earlier.sort_by_key(|s| s.bin);
                    if n > 0 {
                        earlier.push(step);
                    }
                    let again = Instant::now();
                    for step in earlier {
                        let left = timeout.map(|t| t.saturating_sub(again.elapsed()));
                        match self.run_step(bins, step, mutant, &tmp, left)? {
                            Ran::Missed => {}
                            Ran::Caught => return Ok((Ran::Caught, start.elapsed())),
                            _ => break,
                        }
                    }
                    return Ok((Ran::Timeout, start.elapsed()));
                }
                ran => return Ok((ran, start.elapsed())),
            }
        }
        Ok((Ran::Missed, start.elapsed()))
    }

    /// One step with `left` to run: `Caught` when it fails, `Missed` when it
    /// passes, `Timeout` when it does not end in time (or no time is left).
    fn run_step(
        &self,
        bins: &[TestBinary],
        step: &Step,
        mutant: Option<usize>,
        tmp: &Path,
        left: Option<Duration>,
    ) -> Result<Ran> {
        interrupt::check()?;
        if left == Some(Duration::ZERO) {
            return Ok(Ran::Timeout);
        }
        let sink = if self.early_kill {
            Sink::WatchFailed
        } else {
            Sink::Null
        };
        let args = step.args.as_deref().unwrap_or(self.bin_args);
        let mut cmd = self.command(&bins[step.bin], args, mutant, None, tmp);
        let fin = run_group(&mut cmd, left, &sink)?;
        Ok(match fin.status {
            _ if fin.early => Ran::Caught,
            None => Ran::Timeout,
            Some(status) if !status.success() => Ran::Caught,
            Some(_) => Ran::Missed,
        })
    }
}

pub enum Baseline {
    Passed {
        took: Duration,
        /// What each binary took.
        took_each: Vec<Duration>,
        /// Per binary: its output is libtest's (`running N tests`), so it
        /// understands `--list` and `--exact`.
        libtest: Vec<bool>,
    },
    Failed {
        binary: String,
        log_tail: String,
    },
    TimedOut(String),
}

impl Tests<'_> {
    /// Run every binary without a mutant. They have to pass. With `trace`, the
    /// ids of the mutants the tests reach are appended to that file.
    pub fn baseline(
        &self,
        bins: &[TestBinary],
        limit: Option<Duration>,
        trace: Option<&Path>,
    ) -> Result<Baseline> {
        let tmp = self.tmp("baseline")?;
        let start = Instant::now();
        let mut libtest = Vec::new();
        let mut took_each = Vec::new();
        for (n, bin) in bins.iter().enumerate() {
            let log = tmp.join(format!("baseline-{n}.log"));
            let left = limit.map(|t| t.saturating_sub(start.elapsed()));
            let began = Instant::now();
            let fin = run_group(
                &mut self.command(bin, self.bin_args, None, trace, &tmp),
                left,
                &Sink::File(log.clone()),
            )?;
            let name = format!("{} ({})", bin.target, bin.package);
            match fin.status {
                None => return Ok(Baseline::TimedOut(name)),
                Some(status) if !status.success() => {
                    let text = std::fs::read_to_string(&log).unwrap_or_default();
                    let lines: Vec<&str> = text.lines().collect();
                    let log_tail = lines[lines.len().saturating_sub(40)..].join("\n");
                    return Ok(Baseline::Failed {
                        binary: name,
                        log_tail,
                    });
                }
                Some(_) => {}
            }
            took_each.push(began.elapsed());
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            libtest.push(text.lines().any(|l| {
                l.starts_with("running ") && l.ends_with(" tests") || l == "running 1 test"
            }));
        }
        Ok(Baseline::Passed {
            took: start.elapsed(),
            took_each,
            libtest,
        })
    }
}

/// Run the tests for one embedded mutant: the ones routing picks, or all.
fn run_one(
    tests: &Tests,
    bins: &[TestBinary],
    pool: &Pool,
    id: usize,
    job: &str,
    pass: Pass,
) -> Result<(Ran, Duration)> {
    let (plan, estimate) = match pool.routing {
        Some(routing) => {
            let (plan, estimate) = routing.plan_and_estimate(id, bins.len());
            (Some(plan), estimate)
        }
        None => (None, None),
    };
    let limit = pool.limit(pass, estimate);
    match plan {
        None => tests.run(bins, Some(id), job, Some(limit)),
        Some(Plan::Unreached) => Ok((Ran::Unreached, Duration::ZERO)),
        Some(Plan::Steps(steps)) => {
            // A first run of several jobs that reaches its limit is run again.
            let decides = pass == Pass::Alone || pool.jobs < 2;
            tests.run_steps(bins, &steps, Some(id), job, Some(limit), decides)
        }
    }
}

/// How many mutants ended with a time-out run again alone.
pub struct Rerun {
    pub count: usize,
}

/// How the mutants are run.
#[derive(Clone, Copy)]
pub struct Pool<'a> {
    pub jobs: usize,
    /// The limit of a mutant that runs every test, or the explicit limit.
    pub timeout: Duration,
    /// The least a routed mutant gets in the first pass of several jobs
    /// ([`routed_timeout`]). `None` when the limit was given explicitly: every
    /// run gets `timeout` then.
    pub routed_floor: Option<Duration>,
    pub routing: Option<&'a Routing>,
}

/// Which of the two runs of a mutant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// With the other jobs.
    First,
    /// Alone, after a first run that reached its limit. Its result is final.
    Alone,
}

impl Pool<'_> {
    /// The limit of a run whose steps took `estimate` without a mutant.
    ///
    /// The run that decides the result always gets `timeout`: the run alone,
    /// and the only run of a single job. The first run of several jobs is run
    /// again when it reaches its limit, so it may be given up sooner.
    pub fn limit(&self, pass: Pass, estimate: Option<Duration>) -> Duration {
        match (pass, self.routed_floor, estimate) {
            (Pass::First, Some(floor), Some(estimate)) if self.jobs >= 2 => {
                routed_timeout(estimate, floor, self.timeout)
            }
            _ => self.timeout,
        }
    }
}

/// Run the mutants `ids` against the embedded test binaries, `jobs` at a time.
///
/// With two or more jobs a mutant that reaches its limit may only be slow
/// because of the load, or (with routing) because the limit of that first run
/// was made from the time of its own tests: it is run again alone at the end,
/// with `timeout`, and takes the status of that second run. `done` is called
/// once per mutant, with its final status.
pub fn run_embedded(
    tests: &Tests,
    bins: &[TestBinary],
    ids: &[usize],
    pool: &Pool,
    done: &(dyn Fn(usize, Ran, Duration) + Sync),
    rerun_note: &(dyn Fn(Rerun) + Sync),
) -> Result<Vec<(usize, Ran, Duration)>> {
    let jobs = pool.jobs;
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let results: Mutex<Vec<(usize, Ran, Duration)>> = Mutex::new(Vec::new());
    let error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for worker in 0..jobs.min(ids.len()).max(1) {
            let (next, stop, results, error) = (&next, &stop, &results, &error);
            scope.spawn(move || {
                let job = worker.to_string();
                while !stop.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&id) = ids.get(i) else { break };
                    match run_one(tests, bins, pool, id, &job, Pass::First) {
                        Ok((ran, took)) => {
                            if !(ran == Ran::Timeout && jobs >= 2) {
                                done(id, ran, took);
                            }
                            results.lock().expect("results").push((id, ran, took));
                        }
                        Err(e) => {
                            error.lock().expect("error").get_or_insert(e);
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().expect("error") {
        return Err(e);
    }
    let mut results = results.into_inner().expect("results");
    if jobs >= 2 {
        let slow: Vec<usize> = results
            .iter()
            .filter(|r| r.1 == Ran::Timeout)
            .map(|r| r.0)
            .collect();
        if !slow.is_empty() {
            rerun_note(Rerun { count: slow.len() });
        }
        for id in slow {
            let (ran, took) = run_one(tests, bins, pool, id, "0", Pass::Alone)?;
            done(id, ran, took);
            if let Some(r) = results.iter_mut().find(|r| r.0 == id) {
                *r = (id, ran, took);
            }
        }
    }
    results.sort_by_key(|r| r.0);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn timeout_is_five_times_the_baseline_but_at_least_twenty_seconds() {
        assert_eq!(mutant_timeout(secs(1), None), secs(20));
        assert_eq!(mutant_timeout(secs(4), None), secs(20));
        assert_eq!(mutant_timeout(secs(5), None), secs(25));
        assert_eq!(mutant_timeout(Duration::from_millis(4100), None), secs(21));
        assert_eq!(mutant_timeout(Duration::from_millis(3900), None), secs(20));
        assert_eq!(mutant_timeout(secs(100), None), secs(500));
    }

    #[test]
    fn an_explicit_timeout_wins() {
        assert_eq!(mutant_timeout(secs(100), Some(secs(3))), secs(3));
        assert_eq!(mutant_timeout(secs(1), Some(secs(90))), secs(90));
    }

    #[test]
    fn a_routed_mutant_gets_five_times_its_tests_between_ten_seconds_and_the_whole_limit() {
        let ms = Duration::from_millis;
        // most mutants: tests of milliseconds, so the floor
        assert_eq!(
            routed_timeout(Duration::ZERO, secs(10), secs(120)),
            secs(10)
        );
        assert_eq!(routed_timeout(ms(100), secs(10), secs(120)), secs(10));
        assert_eq!(routed_timeout(secs(2), secs(10), secs(120)), secs(10));
        // five times, rounded up to the second
        assert_eq!(routed_timeout(ms(2001), secs(10), secs(120)), secs(11));
        assert_eq!(routed_timeout(secs(3), secs(10), secs(120)), secs(15));
        assert_eq!(routed_timeout(ms(3100), secs(10), secs(120)), secs(16));
        // never more than a mutant that runs every test
        assert_eq!(routed_timeout(secs(23), secs(10), secs(120)), secs(115));
        assert_eq!(routed_timeout(ms(24_100), secs(10), secs(120)), secs(120));
        assert_eq!(routed_timeout(secs(60), secs(10), secs(120)), secs(120));
        assert_eq!(routed_timeout(secs(3), secs(10), secs(20)), secs(15));
        assert_eq!(routed_timeout(secs(5), secs(10), secs(20)), secs(20));
    }

    #[test]
    fn default_jobs_stay_between_one_and_four() {
        assert!((1..=4).contains(&default_jobs()));
    }

    fn sh(script: &str) -> (TestBinary, Vec<String>) {
        (
            TestBinary {
                path: "/bin/sh".into(),
                cwd: "/".into(),
                env: vec![("KIT_VALUE".into(), "kept".into())],
                package: "p".into(),
                target: "t".into(),
                kind: "lib".into(),
            },
            vec!["-c".into(), script.into()],
        )
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("schemata-exec-{name}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scratch_dirs_are_unique_even_for_the_same_name() {
        let a = Scratch::new("same");
        let b = Scratch::new("same");
        assert_ne!(a.0, b.0);
        drop(a);
        assert!(b.0.is_dir(), "dropping one must not remove the other");
    }

    #[test]
    fn the_job_tmpdir_is_emptied_before_each_run() {
        let s = Scratch::new("tmpclear");
        let (bin, args) = sh("test ! -e \"$TMPDIR/left\" && touch \"$TMPDIR/left\"");
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0,
            early_kill: false,
        };
        // The second run would see the first run's file without the clearing.
        assert_eq!(
            tests
                .run(std::slice::from_ref(&bin), None, "j", None)
                .unwrap()
                .0,
            Ran::Missed
        );
        assert_eq!(tests.run(&[bin], None, "j", None).unwrap().0, Ran::Missed);
    }

    fn run_sh(script: &str, mutant: Option<usize>, limit: Option<Duration>) -> (Ran, Duration) {
        let s = Scratch::new("one");
        let (bin, args) = sh(script);
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0,
            early_kill: false,
        };
        tests.run(&[bin], mutant, "0", limit).unwrap()
    }

    #[test]
    fn exit_status_decides_caught_or_missed() {
        assert_eq!(run_sh("exit 0", None, None).0, Ran::Missed);
        assert_eq!(run_sh("exit 3", None, None).0, Ran::Caught);
        assert_eq!(run_sh("kill -9 $$", None, None).0, Ran::Caught);
    }

    #[test]
    fn mutant_and_environment_reach_the_binary() {
        let script = "test \"$CARGO_SCHEMATA_MUTANT\" = 7 && test \"$KIT_VALUE\" = kept && test -d \"$TMPDIR\" && test \"$PWD\" = /";
        assert_eq!(run_sh(script, Some(7), None).0, Ran::Missed);
        assert_eq!(run_sh(script, Some(8), None).0, Ran::Caught);
        // the variable is removed when no mutant is selected
        assert_eq!(
            run_sh("test -z \"${CARGO_SCHEMATA_MUTANT+set}\"", None, None).0,
            Ran::Missed
        );
    }

    #[test]
    fn a_binary_that_takes_too_long_times_out() {
        let (ran, took) = run_sh("sleep 30", Some(1), Some(Duration::from_millis(300)));
        assert_eq!(ran, Ran::Timeout);
        assert!(took < secs(10));
    }

    #[test]
    fn processes_left_behind_are_ended_with_the_group() {
        for (name, script, limit) in [
            (
                "hang",
                "sleep 31 & echo $! > pid.txt; wait",
                Some(Duration::from_millis(500)),
            ),
            ("exit", "sleep 32 & echo $! > pid.txt", None),
        ] {
            let s = Scratch::new(name);
            let (mut bin, args) = sh(script);
            bin.cwd = s.0.to_string_lossy().into_owned();
            let tests = Tests {
                bin_args: &args,
                tmp_root: &s.0.join("tmp"),
                early_kill: false,
            };
            let (ran, _) = tests.run(&[bin], Some(1), "0", limit).unwrap();
            assert_eq!(
                ran,
                if limit.is_some() {
                    Ran::Timeout
                } else {
                    Ran::Missed
                }
            );
            let pid: i32 = std::fs::read_to_string(s.0.join("pid.txt"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let mut gone = false;
            for _ in 0..100 {
                // SAFETY: signal 0 only checks that the process exists.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(gone, "{name}: the grandchild {pid} is still alive");
        }
    }

    #[test]
    fn a_failed_test_line_ends_the_run_and_a_pipe_kept_by_another_session_is_not_waited_for() {
        let s = Scratch::new("early");
        let run = |script: &str, early_kill: bool| {
            let (bin, args) = sh(script);
            let tests = Tests {
                bin_args: &args,
                tmp_root: &s.0,
                early_kill,
            };
            tests.run(&[bin], Some(1), "0", Some(secs(20))).unwrap()
        };
        // a failed line: caught at once, although the process would sleep and exit 0
        let (ran, took) = run(
            "echo 'test a ... ok'; echo 'test b ... FAILED'; sleep 10",
            true,
        );
        assert_eq!(ran, Ran::Caught);
        assert!(took < secs(5), "{took:?}");
        // the same output is not looked at without early kill
        let (ran, _) = run("echo 'test b ... FAILED'", false);
        assert_eq!(ran, Ran::Missed);
        // a descendant in a session of its own keeps stdout open for 6 s
        let (ran, took) = run(
            "perl -MPOSIX -e 'POSIX::setsid(); exec \"sleep\", \"6\"' & exit 0",
            true,
        );
        assert_eq!(ran, Ran::Missed);
        assert!(took < secs(4), "{took:?}");
    }

    #[test]
    fn the_binaries_after_a_failing_one_are_not_started() {
        let s = Scratch::new("order");
        let args = vec![
            "-c".to_string(),
            "if [ \"$ROLE\" = first ]; then exit 1; else touch second-ran; fi".to_string(),
        ];
        let role = |r: &str| {
            let (mut bin, _) = sh("");
            bin.cwd = s.0.to_string_lossy().into_owned();
            bin.env.push(("ROLE".into(), r.into()));
            bin
        };
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0.join("tmp"),
            early_kill: false,
        };
        let (ran, _) = tests
            .run(&[role("first"), role("second")], None, "0", None)
            .unwrap();
        assert_eq!(ran, Ran::Caught);
        assert!(!s.0.join("second-ran").exists());
        let (ran, _) = tests
            .run(&[role("second"), role("second")], None, "0", None)
            .unwrap();
        assert_eq!(ran, Ran::Missed);
        assert!(s.0.join("second-ran").exists());
    }

    #[test]
    fn baseline_reports_a_failure_with_the_end_of_its_output() {
        let s = Scratch::new("baseline");
        let (bin, args) = sh("echo one; echo two; exit 1");
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0,
            early_kill: false,
        };
        match tests.baseline(&[bin], None, None).unwrap() {
            Baseline::Failed { log_tail, binary } => {
                assert_eq!(log_tail, "one\ntwo");
                assert_eq!(binary, "t (p)");
            }
            _ => panic!("expected a failure"),
        }
        let (bin, args) = sh("exit 0");
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0,
            early_kill: false,
        };
        assert!(matches!(
            tests.baseline(&[bin], None, None).unwrap(),
            Baseline::Passed { .. }
        ));
    }

    #[test]
    fn jobs_split_the_mutants_and_timeouts_are_run_again_alone() {
        let s = Scratch::new("pool");
        // mutant 2 is slow only while another run is going on: the marker says whether it ran before
        let script = "case \"$CARGO_SCHEMATA_MUTANT\" in 1) exit 1;; 2) if [ -e seen2 ]; then exit 0; else touch seen2; sleep 30; fi;; *) exit 0;; esac";
        let (mut bin, args) = sh(script);
        bin.cwd = s.0.to_string_lossy().into_owned();
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0.join("tmp"),
            early_kill: false,
        };
        let seen = Mutex::new(Vec::new());
        let reruns = AtomicUsize::new(0);
        let out = run_embedded(
            &tests,
            &[bin],
            &[0, 1, 2, 3],
            &Pool {
                jobs: 2,
                timeout: Duration::from_millis(400),
                routed_floor: None,
                routing: None,
            },
            &|id, ran, _| seen.lock().unwrap().push((id, ran)),
            &|r| reruns.store(r.count, Ordering::Relaxed),
        )
        .unwrap();
        let statuses: Vec<(usize, Ran)> = out.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            statuses,
            [
                (0, Ran::Missed),
                (1, Ran::Caught),
                (2, Ran::Missed),
                (3, Ran::Missed)
            ]
        );
        assert_eq!(reruns.load(Ordering::Relaxed), 1);
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    /// What a routed run with early kill does to three kinds of mutants.
    #[test]
    fn a_failed_line_ends_a_mutant_early_and_a_mutant_without_one_runs_to_its_end() {
        let s = Scratch::new("first-failure");
        let marks = s.0.join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        // 1: a test fails at once, a slow one would still run for 30 s
        // 2: no test fails; the tests take a second
        // 3: the line of the failed test arrives in two writes
        // 4: the binary is ended by a signal without reporting a test
        let script = "case \"$CARGO_SCHEMATA_MUTANT\" in \
            1) echo 'running 2 tests'; echo 'test quick ... FAILED'; sleep 30;; \
            2) echo 'test quick ... ok'; echo 'failures: none, FAILED is only a word here'; \
               sleep 1; echo 'test slow ... ok'; touch ended-2;; \
            3) printf 'test quick ... '; sleep 0.3; echo FAILED; sleep 30;; \
            4) echo 'test quick ... ok'; kill -ABRT $$;; \
            esac";
        let (mut bin, args) = sh(script);
        bin.cwd = marks.to_string_lossy().into_owned();
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0.join("tmp"),
            early_kill: true,
        };
        let pool = Pool {
            jobs: 2,
            timeout: secs(20),
            routed_floor: None,
            routing: None,
        };
        let results =
            run_embedded(&tests, &[bin], &[1, 2, 3, 4], &pool, &|_, _, _| {}, &|_| {}).unwrap();
        let ran: Vec<Ran> = results.iter().map(|r| r.1).collect();
        assert_eq!(ran, [Ran::Caught, Ran::Missed, Ran::Caught, Ran::Caught]);
        // caught long before the slow test would have ended
        assert!(results[0].2 < secs(5), "{:?}", results[0].2);
        assert!(results[2].2 < secs(5), "{:?}", results[2].2);
        // the mutant that no test catches ran all of its tests
        assert!(results[1].2 >= secs(1), "{:?}", results[1].2);
        assert!(marks.join("ended-2").exists());
    }

    /// Steps that are not in the order of the binaries (a routed mutant's are
    /// ordered by time): a step that does not end must not hide a failure in a
    /// binary that the order of the binaries would have run before it.
    #[test]
    fn a_step_that_times_out_lets_the_earlier_binaries_decide() {
        let s = Scratch::new("fallback");
        let args = vec![
            "-c".to_string(),
            "touch ran-$ROLE; case $ROLE in fail*) exit 1;; hang*) sleep 30;; \
             slowpass*) sleep 0.7;; slowfail*) sleep 0.5; exit 1;; esac"
                .to_string(),
        ];
        let role = |r: &str| {
            let (mut bin, _) = sh("");
            bin.cwd = s.0.to_string_lossy().into_owned();
            bin.env.push(("ROLE".into(), r.into()));
            bin
        };
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0.join("tmp"),
            early_kill: false,
        };
        let step = |bin| Step { bin, args: None };
        let limit = Some(secs(1));
        let run = |bins: &[TestBinary], order: &[usize], decides: bool| {
            let steps: Vec<Step> = order.iter().map(|&b| step(b)).collect();
            tests
                .run_steps(bins, &steps, Some(1), "0", limit, decides)
                .unwrap()
                .0
        };
        let ran = |name: &str| s.0.join(format!("ran-{name}")).exists();

        // binary 0 fails and binary 1 hangs; 1 is the quicker one without a mutant
        let bins = [role("fail-a"), role("hang-a")];
        assert_eq!(run(&bins, &[0, 1], true), Ran::Caught);
        assert!(!ran("hang-a"));
        assert_eq!(run(&bins, &[1, 0], true), Ran::Caught);
        // a run that is made again alone does not spend the time
        let bins = [role("fail-b"), role("hang-b")];
        assert_eq!(run(&bins, &[1, 0], false), Ran::Timeout);
        assert!(!ran("fail-b"));
        // the earlier binary passes: the time-out stands
        let bins = [role("pass-c"), role("hang-c")];
        assert_eq!(run(&bins, &[1, 0], true), Ran::Timeout);
        assert!(ran("pass-c"));
        // a later binary would not have run in the order of the binaries either
        let bins = [role("hang-d"), role("fail-d")];
        assert_eq!(run(&bins, &[0, 1], true), Ran::Timeout);
        assert!(!ran("fail-d"));
        // of several earlier ones the first that fails decides; later ones are skipped
        let bins = [
            role("pass-e"),
            role("fail-e"),
            role("hang-e"),
            role("fail-f"),
        ];
        assert_eq!(run(&bins, &[2, 3, 1, 0], true), Ran::Caught);
        assert!(ran("pass-e") && ran("fail-e") && !ran("fail-f"));
        // the quicker binary passes but leaves the other too little of the
        // limit to fail in: that one gets the limit anew
        let bins = [role("slowfail-i"), role("slowpass-i")];
        assert_eq!(run(&bins, &[1, 0], true), Ran::Caught);
        assert_eq!(run(&bins, &[1, 0], false), Ran::Timeout);
        // an earlier one that hangs too: still a time-out
        let bins = [role("hang-g"), role("hang-h")];
        assert_eq!(run(&bins, &[1, 0], true), Ran::Timeout);
        assert!(ran("hang-g"));
    }
}

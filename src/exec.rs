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

/// The least time a mutant gets, as in cargo-mutants 27.1.0.
const MINIMUM_TIMEOUT: Duration = Duration::from_secs(20);
/// What the baseline is multiplied by, as in cargo-mutants 27.1.0.
const TIMEOUT_MULTIPLIER: f64 = 5.0;

/// The time the tests of one mutant get: the explicit limit, or the baseline
/// times five, at least 20 seconds (cargo-mutants 27.1.0, `src/timeouts.rs`).
pub fn mutant_timeout(baseline: Duration, explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or_else(|| {
        MINIMUM_TIMEOUT.max(Duration::from_secs_f64(
            (baseline.as_secs_f64() * TIMEOUT_MULTIPLIER).ceil(),
        ))
    })
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    Caught,
    Missed,
    Timeout,
}

impl Tests<'_> {
    fn command(&self, bin: &TestBinary, mutant: Option<usize>, tmp: &Path) -> Command {
        let mut cmd = Command::new(&bin.path);
        cmd.args(self.bin_args)
            .current_dir(&bin.cwd)
            .env_remove(MUTANT_ENV)
            .envs(bin.env.iter().map(|(k, v)| (k, v)))
            .env("INSTA_UPDATE", "no")
            .env("TMPDIR", tmp);
        if let Some(id) = mutant {
            cmd.env(MUTANT_ENV, id.to_string());
        }
        cmd
    }

    fn tmp(&self, job: &str) -> Result<PathBuf> {
        let tmp = self.tmp_root.join(job);
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
        let tmp = self.tmp(job)?;
        let start = Instant::now();
        for bin in bins {
            interrupt::check()?;
            let left = timeout.map(|t| t.saturating_sub(start.elapsed()));
            if left == Some(Duration::ZERO) {
                return Ok((Ran::Timeout, start.elapsed()));
            }
            let fin = run_group(&mut self.command(bin, mutant, &tmp), left, &Sink::Null)?;
            match fin.status {
                None => return Ok((Ran::Timeout, start.elapsed())),
                Some(status) if !status.success() => return Ok((Ran::Caught, start.elapsed())),
                Some(_) => {}
            }
        }
        Ok((Ran::Missed, start.elapsed()))
    }
}

pub enum Baseline {
    Passed(Duration),
    Failed { binary: String, log_tail: String },
    TimedOut(String),
}

impl Tests<'_> {
    /// Run every binary without a mutant. They have to pass.
    pub fn baseline(&self, bins: &[TestBinary], limit: Option<Duration>) -> Result<Baseline> {
        let tmp = self.tmp("baseline")?;
        let start = Instant::now();
        for (n, bin) in bins.iter().enumerate() {
            let log = tmp.join(format!("baseline-{n}.log"));
            let left = limit.map(|t| t.saturating_sub(start.elapsed()));
            let fin = run_group(
                &mut self.command(bin, None, &tmp),
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
        }
        Ok(Baseline::Passed(start.elapsed()))
    }
}

/// How many mutants ended with a time-out run again alone.
pub struct Rerun {
    pub count: usize,
}

/// Run the mutants `ids` against the embedded test binaries, `jobs` at a time.
///
/// With two or more jobs a mutant that reaches the limit may only be slow
/// because of the load: it is run again alone at the end, with the same limit,
/// and takes the status of that second run. `done` is called once per mutant,
/// with its final status.
pub fn run_embedded(
    tests: &Tests,
    bins: &[TestBinary],
    ids: &[usize],
    jobs: usize,
    timeout: Duration,
    done: &(dyn Fn(usize, Ran, Duration) + Sync),
    rerun_note: &(dyn Fn(Rerun) + Sync),
) -> Result<Vec<(usize, Ran, Duration)>> {
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
                    match tests.run(bins, Some(id), &job, Some(timeout)) {
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
            let (ran, took) = tests.run(bins, Some(id), "0", Some(timeout))?;
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
            },
            vec!["-c".into(), script.into()],
        )
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p =
                std::env::temp_dir().join(format!("schemata-exec-{name}-{}", std::process::id()));
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

    fn run_sh(script: &str, mutant: Option<usize>, limit: Option<Duration>) -> (Ran, Duration) {
        let s = Scratch::new("one");
        let (bin, args) = sh(script);
        let tests = Tests {
            bin_args: &args,
            tmp_root: &s.0,
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
        };
        match tests.baseline(&[bin], None).unwrap() {
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
        };
        assert!(matches!(
            tests.baseline(&[bin], None).unwrap(),
            Baseline::Passed(_)
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
        };
        let seen = Mutex::new(Vec::new());
        let reruns = AtomicUsize::new(0);
        let out = run_embedded(
            &tests,
            &[bin],
            &[0, 1, 2, 3],
            2,
            Duration::from_millis(400),
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
}

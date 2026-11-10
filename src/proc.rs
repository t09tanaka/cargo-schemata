//! Run a child in a process group of its own, and end the whole group when it
//! takes too long, when the run is interrupted, or (for test binaries) when
//! the child has exited but left processes behind.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::interrupt::{self, Interrupted};

/// Where the output of the child goes.
pub enum Sink {
    Null,
    /// Both streams into this file.
    File(PathBuf),
    /// Both streams into memory. Used for cargo, whose children are not
    /// killed after it exits.
    Capture,
    /// Stdout is read line by line, stderr dropped. The group is killed as
    /// soon as libtest reports a test as failed (`test <name> ... FAILED`).
    WatchFailed,
}

pub struct Finished {
    /// `None` when the time limit was reached and the group was killed.
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The group was killed because a test was reported as failed.
    pub early: bool,
}

/// A line of libtest's pretty output that reports a failed test. With
/// `--report-time` the line ends with the time, as in `... FAILED <0.012s>`.
pub fn is_failed_line(line: &str) -> bool {
    if !line.starts_with("test ") {
        return false;
    }
    if line.ends_with(" ... FAILED") {
        return true;
    }
    line.rsplit_once(" ... FAILED ").is_some_and(|(_, time)| {
        time.strip_prefix('<')
            .and_then(|t| t.strip_suffix("s>"))
            .is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
    })
}

fn watch(r: impl Read + Send + 'static, hit: Arc<AtomicBool>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut line = Vec::new();
        loop {
            line.clear();
            match r.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if is_failed_line(String::from_utf8_lossy(&line).trim_end()) {
                        hit.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        Vec::new()
    })
}

fn kill_group(pgid: i32) {
    // SAFETY: killpg has no memory-safety preconditions. ESRCH (nothing left) is fine.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Wait until no process is left in the group `pgid` (`killpg` with signal 0
/// answers ESRCH), for at most `cap`. A process that was sent SIGKILL may still
/// be running for a moment, and writing, until the kernel has torn it down; the
/// directory it writes to must not be removed before. True if the group is gone.
fn group_gone_within(pgid: i32, cap: Duration) -> bool {
    let start = Instant::now();
    loop {
        // SAFETY: signal 0 only checks that the group exists.
        let r = unsafe { libc::killpg(pgid, 0) };
        // 0: the group is there. -1: ESRCH is "nothing left"; EPERM is a group
        // that is there and is not ours.
        if r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        if start.elapsed() >= cap {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// How long the wait loop sleeps between two looks at the child.
const NAP: Duration = Duration::from_millis(2);

/// How long the end of a killed group is waited for.
const GROUP_GONE_CAP: Duration = Duration::from_secs(2);

/// Ends the process group when dropped while armed: an early return (`?`) or
/// a panic in the wait loop must not leave the children running.
struct GroupGuard {
    pgid: i32,
    armed: bool,
}

impl GroupGuard {
    fn new(pgid: i32) -> GroupGuard {
        GroupGuard { pgid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.armed {
            kill_group(self.pgid);
        }
    }
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = r.read_to_end(&mut v);
        v
    })
}

pub fn run_group(cmd: &mut Command, timeout: Option<Duration>, sink: &Sink) -> Result<Finished> {
    cmd.stdin(Stdio::null()).process_group(0);
    match sink {
        Sink::Null => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        Sink::File(path) => {
            let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
            cmd.stdout(f.try_clone()?).stderr(f);
        }
        Sink::Capture => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        Sink::WatchFailed => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::null());
        }
    }
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start {:?}", cmd.get_program()))?;
    let pgid = child.id() as i32;
    let mut guard = GroupGuard::new(pgid);
    let failed = Arc::new(AtomicBool::new(false));
    let out = child.stdout.take().map(|o| {
        if matches!(sink, Sink::WatchFailed) {
            watch(o, failed.clone())
        } else {
            drain(o)
        }
    });
    let err = child.stderr.take().map(drain);
    let kill_leftovers = !matches!(sink, Sink::Capture);
    // Where the output goes (Null / File / WatchFailed) the caller clears the
    // temporary directory next: the killed group must be gone by then.
    let clears_tmp = kill_leftovers;
    let mut early = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            if kill_leftovers {
                kill_group(pgid);
            }
            break Some(status);
        }
        if let Some(code) = interrupt::requested() {
            kill_group(pgid);
            let _ = child.wait();
            guard.disarm();
            if clears_tmp {
                group_gone_within(pgid, GROUP_GONE_CAP);
            }
            return Err(Interrupted(code).into());
        }
        if failed.load(Ordering::Relaxed) {
            kill_group(pgid);
            early = true;
            break Some(child.wait()?);
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            kill_group(pgid);
            child.wait()?;
            break None;
        }
        std::thread::sleep(NAP);
    };
    guard.disarm();
    if clears_tmp {
        group_gone_within(pgid, GROUP_GONE_CAP);
    }
    let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
        h.map(|h| h.join().unwrap_or_default()).unwrap_or_default()
    };
    // The watcher ends at EOF, which a process that left the group (its own
    // session) and kept our pipe never gives: it is not waited for.
    let out = if matches!(sink, Sink::WatchFailed) {
        drop(out);
        None
    } else {
        out
    };
    Ok(Finished {
        status,
        stdout: join(out),
        stderr: join(err),
        early,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn sleeper() -> std::process::Child {
        Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn failed_lines_are_recognized_with_and_without_the_time() {
        assert!(is_failed_line("test a::b ... FAILED"));
        assert!(is_failed_line("test a::b ... FAILED <0.012s>"));
        assert!(!is_failed_line("test a::b ... ok <0.012s>"));
        assert!(!is_failed_line("test a::b ... FAILED <soon>"));
        assert!(!is_failed_line("test a::b ... FAILED later"));
        assert!(!is_failed_line("test a::b ... FAILED <s>"));
        assert!(!is_failed_line("a child said ... FAILED"));
    }

    #[test]
    fn dropping_an_armed_guard_kills_the_group() {
        let mut child = sleeper();
        drop(GroupGuard::new(child.id() as i32));
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn a_disarmed_guard_leaves_the_group_alone() {
        let mut child = sleeper();
        let mut guard = GroupGuard::new(child.id() as i32);
        guard.disarm();
        drop(guard);
        // a kill would have been delivered by now
        std::thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_panic_while_the_guard_is_held_kills_the_group() {
        let mut child = sleeper();
        let pgid = child.id() as i32;
        let r = std::panic::catch_unwind(|| {
            let _guard = GroupGuard::new(pgid);
            panic!("boom");
        });
        assert!(r.is_err());
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    fn group_exists(pgid: i32) -> bool {
        // SAFETY: signal 0 only checks that the group exists.
        let r = unsafe { libc::killpg(pgid, 0) };
        !(r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
    }

    #[test]
    fn a_killed_group_is_waited_for_until_it_is_gone() {
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 60 & sleep 60 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = leader.id() as i32;
        assert!(group_exists(pgid));
        kill_group(pgid);
        leader.wait().unwrap();
        assert!(group_gone_within(pgid, Duration::from_secs(5)));
        assert!(!group_exists(pgid));
    }

    #[test]
    fn waiting_for_a_live_group_stops_at_the_cap() {
        let mut child = sleeper();
        let pgid = child.id() as i32;
        let start = Instant::now();
        assert!(!group_gone_within(pgid, Duration::from_millis(60)));
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(60), "{took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
        // a group that is there but is not ours (EPERM, or ours as root) is not gone
        assert!(!group_gone_within(1, Duration::from_millis(10)));
        kill_group(pgid);
        child.wait().unwrap();
        // a group that is not there is gone at once
        assert!(group_gone_within(pgid, Duration::ZERO));
    }

    /// Run a shell that leaves a background process behind and exits; the
    /// process group id (the shell's pid) is written to `pidfile`.
    fn leave_a_child(sink: &Sink, pidfile: &std::path::Path) -> i32 {
        let script = format!(
            "echo $$ > {}; sleep 60 >/dev/null 2>&1 & exit 0",
            pidfile.display()
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        run_group(&mut cmd, None, sink).unwrap();
        std::fs::read_to_string(pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[test]
    fn the_group_is_gone_when_run_group_returns_for_sinks_whose_tmp_is_cleared() {
        let dir = std::env::temp_dir().join(format!("schemata-fix2-proc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sinks = [Sink::Null, Sink::File(dir.join("out")), Sink::WatchFailed];
        for (i, sink) in sinks.iter().enumerate() {
            let pgid = leave_a_child(sink, &dir.join(format!("pid{i}")));
            assert!(!group_exists(pgid), "sink {i}: group {pgid} is still there");
        }
        // Capture does not kill what is left behind, and does not wait for it
        let pgid = leave_a_child(&Sink::Capture, &dir.join("pid-capture"));
        assert!(group_exists(pgid));
        kill_group(pgid);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

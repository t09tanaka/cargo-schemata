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

/// A line of libtest's pretty output that reports a failed test.
pub fn is_failed_line(line: &str) -> bool {
    line.starts_with("test ") && line.ends_with(" ... FAILED")
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
    let mut nap = Duration::from_millis(1);
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
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(20));
    };
    let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
        h.map(|h| h.join().unwrap_or_default()).unwrap_or_default()
    };
    Ok(Finished {
        status,
        stdout: join(out),
        stderr: join(err),
        early,
    })
}

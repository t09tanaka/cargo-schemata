//! SIGINT, SIGTERM and SIGHUP: noted in a flag that every wait loop polls, so that the
//! children (which are in process groups of their own, and do not get a
//! Ctrl-C from the terminal) are stopped by this program before it exits.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

static FLAG: std::sync::OnceLock<Arc<AtomicUsize>> = std::sync::OnceLock::new();

/// The exit code for a signal that stopped the run: 130 for SIGINT, 143 for SIGTERM, 129 for SIGHUP.
#[derive(Debug)]
pub struct Interrupted(pub i32);

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupted")
    }
}

impl std::error::Error for Interrupted {}

/// The exit code for each signal that is noted: 128 plus its number.
const EXIT_CODES: [(i32, usize); 3] = [(SIGINT, 130), (SIGTERM, 143), (SIGHUP, 129)];

/// Start noting the signals. Without this, a signal ends the process at once.
pub fn install() -> std::io::Result<()> {
    let flag = Arc::new(AtomicUsize::new(0));
    for (signal, code) in EXIT_CODES {
        signal_hook::flag::register_usize(signal, Arc::clone(&flag), code)?;
    }
    let _ = FLAG.set(flag);
    Ok(())
}

/// The exit code of the signal that arrived, if one did.
pub fn requested() -> Option<i32> {
    let code = FLAG.get()?.load(Ordering::Relaxed);
    (code != 0).then_some(code as i32)
}

pub fn check() -> Result<(), Interrupted> {
    match requested() {
        Some(code) => Err(Interrupted(code)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_128_plus_the_signal() {
        for (signal, code) in EXIT_CODES {
            assert_eq!(code, 128 + signal as usize);
        }
        assert!(EXIT_CODES.iter().any(|&(s, c)| s == SIGHUP && c == 129));
    }
}

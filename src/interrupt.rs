//! SIGINT and SIGTERM: noted in a flag that every wait loop polls, so that the
//! children (which are in process groups of their own, and do not get a
//! Ctrl-C from the terminal) are stopped by this program before it exits.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use signal_hook::consts::{SIGINT, SIGTERM};

static FLAG: std::sync::OnceLock<Arc<AtomicUsize>> = std::sync::OnceLock::new();

/// The exit code for a signal that stopped the run: 130 for SIGINT, 143 for SIGTERM.
#[derive(Debug)]
pub struct Interrupted(pub i32);

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "interrupted")
    }
}

impl std::error::Error for Interrupted {}

/// Start noting the signals. Without this, a signal ends the process at once.
pub fn install() -> std::io::Result<()> {
    let flag = Arc::new(AtomicUsize::new(0));
    signal_hook::flag::register_usize(SIGINT, Arc::clone(&flag), 130)?;
    signal_hook::flag::register_usize(SIGTERM, Arc::clone(&flag), 143)?;
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

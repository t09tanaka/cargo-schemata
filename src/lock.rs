//! One run at a time in a package: an advisory lock on
//! `<target dir>/schemata/lock`, held for as long as the process lives. The
//! operating system gives it up when the process ends however it ends, so a
//! killed run leaves no stale lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

#[derive(Debug)]
pub struct Lock {
    _file: File,
}

impl Lock {
    pub fn acquire(schemata_dir: &Path) -> Result<Lock> {
        std::fs::create_dir_all(schemata_dir)
            .with_context(|| format!("create {}", schemata_dir.display()))?;
        let path = schemata_dir.join("lock");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut who = String::new();
                let _ = file.read_to_string(&mut who);
                let who = who.trim().replace('\n', ", ");
                let who = if who.is_empty() {
                    String::new()
                } else {
                    format!(" ({who})")
                };
                bail!(
                    "another cargo schemata run is at work in this package{who}. Nothing was run or changed; run again when it has ended (the lock is {})",
                    path.display()
                );
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("cannot lock {}", path.display()));
            }
        }
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        file.set_len(0)?;
        file.rewind()?;
        write!(
            file,
            "pid {}\nstarted at unix time {started}\n",
            std::process::id()
        )?;
        file.flush()?;
        Ok(Lock { _file: file })
    }
}

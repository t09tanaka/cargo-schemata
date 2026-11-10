//! The temporary directory of a run, and removing directories that tests made
//! read-only.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Give the owner full access to `path` and everything below it, so that it can
/// be listed and deleted. Symbolic links are left alone.
fn make_writable(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o700);
    let _ = std::fs::set_permissions(path, perms);
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        make_writable(&entry.path());
    }
}

/// `remove_dir_all` that first restores write permission below `path`: a test
/// that was killed can leave read-only directories, which cannot be deleted
/// as they are. A path that is not there is not an error.
pub fn force_remove_dir_all(path: &Path) -> io::Result<()> {
    // A process of a killed test may still be writing into it for a moment
    // ("directory not empty"), so a failure is tried again a few times.
    let mut attempt = 0u32;
    loop {
        make_writable(path);
        match std::fs::remove_dir_all(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if attempt >= 4 => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25 << attempt)),
            Ok(()) => return Ok(()),
        }
        attempt += 1;
    }
}

/// A directory of its own for one run, below the temporary directory of the
/// system (so outside the repository under test), removed when this is dropped.
pub struct RunTmp {
    path: PathBuf,
}

impl RunTmp {
    pub fn create() -> Result<RunTmp> {
        let base = std::env::temp_dir();
        std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for attempt in 0..100u32 {
            let path = base.join(format!(
                "cargo-schemata-{}-{nanos:x}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(RunTmp { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        }
        anyhow::bail!(
            "no unused name for a temporary directory in {}",
            base.display()
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RunTmp {
    fn drop(&mut self) {
        let _ = force_remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_read_only_directories_and_a_missing_path() {
        let tmp = RunTmp::create().unwrap();
        let ro = tmp.path().join("a/ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("f"), "x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(std::fs::remove_dir_all(tmp.path().join("a")).is_err());
        force_remove_dir_all(&tmp.path().join("a")).unwrap();
        assert!(!tmp.path().join("a").exists());
        force_remove_dir_all(&tmp.path().join("a")).unwrap();
    }

    #[test]
    fn a_run_directory_is_outside_the_cwd_and_removed_on_drop() {
        let a = RunTmp::create().unwrap();
        let b = RunTmp::create().unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().starts_with(std::env::temp_dir()));
        let p = a.path().to_owned();
        std::fs::create_dir_all(p.join("x")).unwrap();
        std::fs::set_permissions(p.join("x"), std::fs::Permissions::from_mode(0o000)).unwrap();
        drop(a);
        assert!(!p.exists());
    }
}

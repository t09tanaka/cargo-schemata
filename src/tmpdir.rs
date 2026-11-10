//! The temporary directory of a run, and removing directories that tests made
//! read-only.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

const PREFIX: &str = "cargo-schemata-";

/// The flags to open a directory and never a link (a link is `ELOOP`, a file
/// `ENOTDIR`). The bits are distinct, so the sum is their union.
fn open_flags() -> libc::c_int {
    [
        libc::O_RDONLY,
        libc::O_NOFOLLOW,
        libc::O_DIRECTORY,
        libc::O_CLOEXEC,
    ]
    .iter()
    .sum()
}

/// Open the directory `name` below `dir` (a descriptor, or `AT_FDCWD`) without
/// following a symbolic link. A directory that cannot be opened for lack of
/// permission is made accessible first, again without following links.
fn open_dir_at(dir: libc::c_int, name: &CString) -> Option<OwnedFd> {
    let open = || {
        // SAFETY: `name` is a valid C string; the result is checked.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), open_flags()) };
        // SAFETY: a non-negative `fd` is a new descriptor that nothing else owns.
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    };
    if let Some(fd) = open() {
        return Some(fd);
    }
    if io::Error::last_os_error().raw_os_error() != Some(libc::EACCES) {
        return None;
    }
    // SAFETY: `name` is valid.
    let r = unsafe { libc::fchmodat(dir, name.as_ptr(), 0o700, libc::AT_SYMLINK_NOFOLLOW) };
    if r != 0 {
        return None;
    }
    open()
}

/// Give the owner full access to the directory `fd` and everything below it.
fn make_writable_fd(fd: OwnedFd) {
    // SAFETY: `fd` is an open directory.
    unsafe { libc::fchmod(fd.as_raw_fd(), 0o700) };
    let Ok(dup) = fd.try_clone() else {
        return;
    };
    let raw = dup.into_raw_fd();
    // SAFETY: `raw` is a directory descriptor that fdopendir takes over (and
    // closes with closedir on success).
    let dirp = unsafe { libc::fdopendir(raw) };
    if dirp.is_null() {
        // SAFETY: fdopendir failed, so `raw` is still ours to close.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
        return;
    }
    let mut names: Vec<CString> = Vec::new();
    loop {
        // SAFETY: `dirp` is an open directory stream.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: readdir returns a valid entry with a NUL-terminated name.
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        if !matches!(name.to_bytes(), b"." | b"..") {
            names.push(name.to_owned());
        }
    }
    // SAFETY: `dirp` came from fdopendir and is closed once.
    unsafe { libc::closedir(dirp) };
    // What is not a directory (or is a link) cannot be opened and is left alone.
    for name in names {
        if let Some(child) = open_dir_at(fd.as_raw_fd(), &name) {
            make_writable_fd(child);
        }
    }
}

/// Give the owner full access to `path` and everything below it, so that it can
/// be listed and deleted. Symbolic links are never followed (and never
/// changed), even when one is put in place while this runs.
fn make_writable(path: &Path) {
    let Ok(name) = CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    if let Some(fd) = open_dir_at(libc::AT_FDCWD, &name) {
        make_writable_fd(fd);
    }
}

/// Call `remove` until it succeeds. A path that is not there is not an error,
/// and the only error that is tried again is "directory not empty" (a process
/// of a killed test may still be writing into it for a moment): up to four more
/// times, after 25, 50, 100 and 200 ms. Any other error is returned at once.
pub fn remove_with_retry(
    path: &Path,
    mut remove: impl FnMut(&Path) -> io::Result<()>,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let mut attempt = 0u32;
    loop {
        match remove(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty && attempt < 4 => {
                sleep(Duration::from_millis(25 << attempt));
            }
            Err(e) => return Err(e),
        }
        attempt += 1;
    }
}

/// `remove_dir_all` that first restores write permission below `path`: a test
/// that was killed can leave read-only directories, which cannot be deleted
/// as they are. A path that is not there is not an error.
pub fn force_remove_dir_all(path: &Path) -> io::Result<()> {
    remove_with_retry(
        path,
        |p| {
            make_writable(p);
            std::fs::remove_dir_all(p)
        },
        std::thread::sleep,
    )
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid cannot fail and has no preconditions.
    unsafe { libc::geteuid() }
}

fn pid_is_dead(pid: u32) -> bool {
    // pid 0 and values that do not fit would address a group or everything.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// The pid in the name of a run directory, `cargo-schemata-<pid>-<hex>-<n>`,
/// when the name is exactly that.
fn pid_of_run_dir(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(PREFIX)?;
    let mut parts = rest.splitn(3, '-');
    let (pid, stamp, n) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid)
        || !digits(n)
        || stamp.is_empty()
        || !stamp.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    pid.parse().ok()
}

/// Remove the run directories in `base` of runs that were killed (their pid is
/// gone), so that a SIGKILL does not leave them behind for good. Only
/// directories of this exact name that belong to `uid` are taken; errors are
/// ignored.
fn sweep_stale(base: &Path, uid: u32, pid_dead: impl Fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(pid_of_run_dir) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() && meta.uid() == uid && pid_dead(pid) {
            let _ = force_remove_dir_all(&entry.path());
        }
    }
}

/// Create a directory for `pid` below `base` that nothing else has: the owner
/// alone may use it. A name that is taken is not reused.
fn create_unique(base: &Path, pid: u32, nanos: u32) -> Result<PathBuf> {
    for attempt in 0..100u32 {
        let path = base.join(format!("{PREFIX}{pid}-{nanos:x}-{attempt}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
        }
    }
    anyhow::bail!(
        "no unused name for a temporary directory in {}",
        base.display()
    )
}

/// What to tell the user when a directory could not be removed.
fn cleanup_failure(path: &Path, result: &io::Result<()>) -> Option<String> {
    result.as_ref().err().map(|e| {
        format!(
            "cargo schemata: could not remove the temporary directory {}: {e}",
            path.display()
        )
    })
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
        sweep_stale(&base, effective_uid(), pid_is_dead);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = create_unique(&base, std::process::id(), nanos)?;
        Ok(RunTmp { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RunTmp {
    fn drop(&mut self) {
        let result = force_remove_dir_all(&self.path);
        if let Some(message) = cleanup_failure(&self.path, &result) {
            eprintln!("{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn removes_read_only_directories_and_a_missing_path() {
        let tmp = RunTmp::create().unwrap();
        let ro = tmp.path().join("a/ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("f"), "x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        if effective_uid() != 0 {
            // root can delete a read-only directory, so there is nothing to show
            assert!(std::fs::remove_dir_all(tmp.path().join("a")).is_err());
        }
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

    fn io_err(kind: io::ErrorKind) -> io::Error {
        io::Error::from(kind)
    }

    /// Run `remove_with_retry` with `results` as the answers of `remove`.
    fn retry(results: Vec<io::Result<()>>) -> (io::Result<()>, usize, Vec<u64>) {
        let mut results = results.into_iter();
        let mut calls = 0;
        let mut sleeps = Vec::new();
        let r = remove_with_retry(
            Path::new("x"),
            |_| {
                calls += 1;
                results.next().expect("called more often than expected")
            },
            |d| sleeps.push(d.as_millis() as u64),
        );
        (r, calls, sleeps)
    }

    #[test]
    fn a_directory_that_is_not_empty_is_retried_with_growing_pauses() {
        let busy = || Err(io_err(io::ErrorKind::DirectoryNotEmpty));
        let (r, calls, sleeps) = retry(vec![busy(), busy(), busy(), Ok(())]);
        assert!(r.is_ok());
        assert_eq!((calls, sleeps), (4, vec![25, 50, 100]));

        let (r, calls, sleeps) = retry(vec![busy(), busy(), busy(), busy(), busy()]);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::DirectoryNotEmpty);
        assert_eq!((calls, sleeps), (5, vec![25, 50, 100, 200]));
    }

    #[test]
    fn a_missing_path_and_other_errors_are_not_retried() {
        let (r, calls, sleeps) = retry(vec![Err(io_err(io::ErrorKind::NotFound))]);
        assert!(r.is_ok());
        assert_eq!((calls, sleeps), (1, vec![]));

        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let (r, calls, sleeps) = retry(vec![Err(io_err(kind))]);
            assert_eq!(r.unwrap_err().kind(), kind);
            assert_eq!((calls, sleeps), (1, vec![]));
        }
        let (r, calls, _) = retry(vec![Err(io_err(io::ErrorKind::DirectoryNotEmpty)), Ok(())]);
        assert!(r.is_ok());
        assert_eq!(calls, 2);
    }

    /// A pid that is certainly not in use: a child that has been waited for.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn run_dir_name(pid: u32) -> String {
        format!("{PREFIX}{pid}-1f2e-0")
    }

    #[test]
    fn stale_run_directories_of_dead_runs_are_swept_and_live_ones_kept() {
        let tmp = RunTmp::create().unwrap();
        let base = tmp.path();
        let dead = base.join(run_dir_name(dead_pid()));
        let live = base.join(run_dir_name(std::process::id()));
        for d in [&dead, &live] {
            std::fs::create_dir_all(d.join("ro")).unwrap();
            std::fs::set_permissions(d.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        sweep_stale(base, effective_uid(), pid_is_dead);
        assert!(!dead.exists());
        assert!(live.exists());
    }

    #[test]
    fn the_sweep_takes_only_exact_names_directories_and_own_files() {
        let tmp = RunTmp::create().unwrap();
        let base = tmp.path();
        let pid = dead_pid();
        let others = [
            format!("{PREFIX}{pid}"),
            format!("{PREFIX}{pid}-1f2e"),
            format!("{PREFIX}{pid}-xyz-0"),
            format!("{PREFIX}{pid}-1f2e-0-extra"),
            format!("{PREFIX}{pid}--0"),
            format!("{PREFIX}-1f2e-0"),
            format!("other-{pid}-1f2e-0"),
        ];
        for name in &others {
            std::fs::create_dir(base.join(name)).unwrap();
        }
        // a file and a symlink with the right name are not directories
        let file = base.join(format!("{PREFIX}{pid}-aa-1"));
        std::fs::write(&file, "x").unwrap();
        let target = base.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = base.join(format!("{PREFIX}{pid}-aa-2"));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // a directory that belongs to someone else
        let foreign = base.join(format!("{PREFIX}{pid}-aa-3"));
        std::fs::create_dir(&foreign).unwrap();

        sweep_stale(base, effective_uid(), pid_is_dead);
        for name in &others {
            assert!(base.join(name).exists(), "{name}");
        }
        assert!(file.exists() && link.exists() && target.exists());
        assert!(!foreign.exists(), "own directory with the exact name goes");

        let foreign = base.join(format!("{PREFIX}{pid}-aa-4"));
        std::fs::create_dir(&foreign).unwrap();
        sweep_stale(base, effective_uid() + 1, pid_is_dead);
        assert!(foreign.exists());
    }

    #[test]
    fn pid_zero_and_out_of_range_are_never_dead() {
        assert!(!pid_is_dead(0));
        assert!(!pid_is_dead(u32::MAX));
        assert!(!pid_is_dead(std::process::id()));
        assert!(pid_is_dead(dead_pid()));
    }

    #[test]
    fn the_run_directory_is_private_and_a_stale_one_is_swept_on_create() {
        let a = RunTmp::create().unwrap();
        let mode = std::fs::metadata(a.path()).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let stale = std::env::temp_dir().join(format!("{PREFIX}{}-aa-9", dead_pid()));
        std::fs::create_dir(&stale).unwrap();
        let _b = RunTmp::create().unwrap();
        assert!(!stale.exists());
        assert!(a.path().exists());
    }

    #[test]
    fn a_name_that_is_taken_is_not_reused() {
        let tmp = RunTmp::create().unwrap();
        let base = tmp.path();
        for n in 0..2 {
            std::fs::create_dir(base.join(format!("{PREFIX}7-ab-{n}"))).unwrap();
        }
        let p = create_unique(base, 7, 0xab).unwrap();
        assert_eq!(p, base.join(format!("{PREFIX}7-ab-2")));
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o700);

        for n in 3..100 {
            std::fs::create_dir(base.join(format!("{PREFIX}7-ab-{n}"))).unwrap();
        }
        let err = create_unique(base, 7, 0xab).unwrap_err().to_string();
        assert!(err.contains("no unused name"), "{err}");
        // any other error is returned at once
        let err = create_unique(&base.join("missing/dir"), 7, 0xab)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("create ") && !err.contains("no unused"),
            "{err}"
        );
    }

    #[test]
    fn a_failed_cleanup_is_reported() {
        let p = Path::new("/x/y");
        assert_eq!(cleanup_failure(p, &Ok(())), None);
        let m = cleanup_failure(p, &Err(io_err(io::ErrorKind::PermissionDenied))).unwrap();
        assert!(m.starts_with("cargo schemata: could not remove"), "{m}");
        assert!(m.contains("/x/y"), "{m}");
    }

    #[test]
    fn a_link_inside_is_not_followed_when_permissions_are_restored() {
        let outside = RunTmp::create().unwrap();
        let target = outside.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("f"), "keep").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o555)).unwrap();

        let inside = outside.path().join("inside");
        std::fs::create_dir_all(inside.join("ro/deep")).unwrap();
        std::fs::write(inside.join("ro/deep/f"), "x").unwrap();
        std::fs::set_permissions(
            inside.join("ro/deep"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, inside.join("link")).unwrap();
        std::fs::set_permissions(inside.join("ro"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        force_remove_dir_all(&inside).unwrap();
        assert!(!inside.exists());

        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o555);
        assert_eq!(std::fs::read_to_string(target.join("f")).unwrap(), "keep");
        // and a link that is the path itself is not entered either
        let link = outside.path().join("top");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        make_writable(&link);
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o555);
    }
}

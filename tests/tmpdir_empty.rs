//! Where the tests of a run keep their temporary files and what becomes of
//! them, and a `--file` glob that matches nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        if e.file_name() == "target" {
            continue;
        }
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

struct Kit {
    base: PathBuf,
    dir: PathBuf,
    out: PathBuf,
    record: PathBuf,
}

impl Kit {
    fn new(name: &str) -> Kit {
        let base = std::env::temp_dir().join(format!("schemata-t-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("kit");
        copy_dir(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmp_kit"),
            &dir,
        );
        Kit {
            out: base.join("out"),
            record: base.join("record.txt"),
            base,
            dir,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"));
        cmd.args(["schemata", "-d"])
            .arg(&self.dir)
            .arg("-o")
            .arg(&self.out)
            .args(["-j", "1"])
            .args(args)
            .env("TMP_KIT_RECORD", &self.record)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS");
        cmd
    }

    /// The `TMPDIR` of each test process that ran.
    fn tmpdirs(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(&self.record)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect()
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        make_writable(&self.base);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        for e in std::fs::read_dir(path).into_iter().flatten().flatten() {
            make_writable(&e.path());
        }
    }
}

#[test]
fn temporary_files_are_outside_the_repository_and_removed_after_the_run() {
    let kit = Kit::new("tmp");
    let out = kit.command(&[]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // the test leaves a read-only directory in every TMPDIR it gets; the run
    // must still clear it for the next job and at the end
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let tmpdirs = kit.tmpdirs();
    assert!(tmpdirs.len() >= 2, "{tmpdirs:?}");
    for tmp in &tmpdirs {
        assert!(!tmp.starts_with(&kit.dir), "inside the repository: {tmp:?}");
        assert!(tmp.starts_with(std::env::temp_dir()), "{tmp:?}");
    }
    // one directory for the run, and it is gone
    let root = tmpdirs[0].parent().unwrap();
    assert!(tmpdirs.iter().all(|t| t.parent() == Some(root)));
    assert!(!root.exists(), "left behind: {root:?}");
    assert!(!kit.dir.join("target/schemata/tmp").exists());
}

#[test]
fn a_directory_left_by_an_earlier_version_is_cleared() {
    let kit = Kit::new("legacy");
    let old = kit.dir.join("target/schemata/tmp/0/ro");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("f"), "x").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let out = kit.command(&[]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(!kit.dir.join("target/schemata/tmp").exists());
}

#[test]
fn a_file_glob_that_matches_no_mutant_fails_the_run() {
    let kit = Kit::new("empty");
    let out = kit.command(&["--file", "src/renamed.rs"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("src/renamed.rs"), "{stderr}");
    assert!(stderr.contains("--allow-empty"), "{stderr}");
    assert!(!kit.record.exists(), "nothing should have run");

    // one glob that matches and one that does not: the second is named, the first is not
    let out = kit
        .command(&["--file", "src/lib.rs", "-f", "gone.rs"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("--file gone.rs"), "{stderr}");
    assert!(!stderr.contains("--file src/lib.rs"), "{stderr}");

    // with every glob matching, the run goes on as before
    let out = kit
        .command(&["--file", "src/lib.rs", "-f", "lib.rs"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn allow_empty_lets_a_glob_match_nothing() {
    let kit = Kit::new("allow");
    let out = kit
        .command(&["--file", "src/renamed.rs", "--allow-empty"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no mutants to run"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn an_examine_glob_of_the_config_file_that_matches_nothing_fails_the_run() {
    let kit = Kit::new("config-empty");
    std::fs::create_dir_all(kit.dir.join(".cargo")).unwrap();
    std::fs::write(
        kit.dir.join(".cargo/mutants.toml"),
        "examine_globs = [\"src/lib.rs\", \"src/renamed.rs\"]\n",
    )
    .unwrap();
    let out = kit.command(&[]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("config examine_globs src/renamed.rs"),
        "{stderr}"
    );
    assert!(!stderr.contains("examine_globs src/lib.rs"), "{stderr}");
    assert!(!kit.record.exists(), "nothing should have run");

    // --file and the config file are told apart
    let out = kit.command(&["--file", "gone.rs"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--file gone.rs"), "{stderr}");
    assert!(
        stderr.contains("config examine_globs src/renamed.rs"),
        "{stderr}"
    );

    // --allow-empty covers both
    let out = kit.command(&["--allow-empty"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_empty_glob_exit_clears_the_report_of_an_earlier_run() {
    let kit = Kit::new("clear-report");
    let out = kit.command(&[]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let report = kit.out.join("report.json");
    assert!(
        report.exists(),
        "{:?}",
        std::fs::read_dir(&kit.out).map(|d| d.count())
    );
    let out = kit.command(&["--file", "src/renamed.rs"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!report.exists(), "the old report is still there");
}

#[test]
fn list_and_build_only_do_not_run_the_empty_check() {
    let kit = Kit::new("list-empty");
    for mode in ["--list", "--build-only"] {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
            .args(["schemata", "-d"])
            .arg(&kit.dir)
            .args([mode, "--file", "src/renamed.rs"])
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn a_glob_that_matches_only_excluded_files_is_empty() {
    let kit = Kit::new("excluded");
    let out = kit
        .command(&["--file", "src/lib.rs", "--exclude", "src/lib.rs"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("--file src/lib.rs"), "{stderr}");
}

#[test]
fn the_empty_check_looks_at_the_tree_not_at_the_diff() {
    let kit = Kit::new("diff-empty");
    let diff = kit.base.join("empty.diff");
    std::fs::write(&diff, "").unwrap();
    let diff = diff.to_str().unwrap().to_owned();
    // an empty diff leaves nothing to test, but a glob of a missing file still fails
    let out = kit
        .command(&["--file", "src/renamed.rs", "--in-diff", &diff])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    // and a glob of a file the diff does not touch does not
    let out = kit
        .command(&["--file", "src/lib.rs", "--in-diff", &diff])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

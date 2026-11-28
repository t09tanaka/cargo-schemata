//! Inside a workspace member, `--file` and `--exclude` globs may name a path
//! relative to the member as well as to the workspace root, and the member's
//! own `.cargo/mutants.toml` applies. The listed paths stay relative to the
//! workspace root.

use std::path::PathBuf;
use std::process::Command;

fn kit() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/member_kit")
}

fn list(dir: &str, args: &[&str]) -> (Vec<String>, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
        .arg("schemata")
        .arg("--list")
        .args(args)
        .current_dir(kit().join(dir))
        .output()
        .expect("run cargo-schemata");
    let stdout = String::from_utf8(out.stdout).unwrap();
    (
        stdout.lines().map(ToOwned::to_owned).collect(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap(),
    )
}

fn files(lines: &[String]) -> Vec<&str> {
    let mut f: Vec<&str> = lines.iter().map(|l| l.split(':').next().unwrap()).collect();
    f.dedup();
    f
}

#[test]
fn package_relative_file_glob_matches_in_member() {
    let (lines, err, code) = list("alpha", &["--file", "src/extra.rs"]);
    assert_eq!(code, 0, "{err}");
    assert!(!lines.is_empty(), "{err}");
    assert_eq!(files(&lines), ["alpha/src/extra.rs"]);
}

#[test]
fn root_relative_file_glob_still_matches_in_member() {
    let (lines, err, code) = list("alpha", &["--file", "alpha/src/extra.rs"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(files(&lines), ["alpha/src/extra.rs"]);
    let (same, _, _) = list("alpha", &["--file", "src/extra.rs"]);
    assert_eq!(lines, same);
}

#[test]
fn package_relative_exclude_glob_applies_in_member() {
    let (lines, err, code) = list("alpha", &["--exclude", "src/extra.rs"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(files(&lines), ["alpha/src/lib.rs"]);
}

#[test]
fn member_config_applies_and_explicit_config_wins() {
    let (lines, err, code) = list("alpha", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|l| !l.contains("drop_me")), "{lines:?}");
    let (lines, err, code) = list("alpha", &["--no-config"]);
    assert_eq!(code, 0, "{err}");
    assert!(lines.iter().any(|l| l.contains("drop_me")), "{lines:?}");
    // -p from the root selects the member too.
    let (lines, err, code) = list(".", &["-p", "alpha"]);
    assert_eq!(code, 0, "{err}");
    assert!(lines.iter().all(|l| !l.contains("drop_me")), "{lines:?}");
}

#[test]
fn several_packages_keep_root_config_and_root_paths() {
    let (lines, err, code) = list(".", &["--workspace"]);
    assert_eq!(code, 0, "{err}");
    assert!(lines.iter().any(|l| l.contains("drop_me")), "{lines:?}");
    let (lines, _, _) = list(".", &["--workspace", "--file", "src/extra.rs"]);
    assert!(lines.is_empty(), "{lines:?}");
}

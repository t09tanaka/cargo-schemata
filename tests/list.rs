//! `cargo schemata --list` must agree with `cargo mutants --list` (27.1.0) on
//! the fixture crates. The expected files were produced by cargo-mutants itself;
//! see `tests/fixtures/cases.json` for the invocations.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn run(dir: &str, args: &[String]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
        .args(args)
        .current_dir(fixtures().join(dir))
        .output()
        .expect("run cargo-schemata");
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap(),
    )
}

/// The only intended difference: the comment each tool leaves in the diff.
fn same_marker(s: &str) -> String {
    s.replace("cargo-schemata", "cargo-mutants")
}

#[test]
fn list_matches_cargo_mutants_on_fixtures() {
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("cases.json")).unwrap())
            .unwrap();
    assert!(cases.len() >= 30);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let text = case["text"].as_bool().unwrap_or(false);
        let mut args: Vec<String> = vec!["schemata".into(), "--list".into()];
        if !text {
            args.push("--json".into());
        }
        args.extend(case["args"].as_array().unwrap().iter().map(|a| {
            a.as_str()
                .unwrap()
                .replace("@fixtures@", fixtures().to_str().unwrap())
        }));
        let (stdout, stderr, code) = run(case["dir"].as_str().unwrap(), &args);
        assert_eq!(code, 0, "{name}: {stderr}");
        let ext = if text { "txt" } else { "json" };
        let expected =
            std::fs::read_to_string(fixtures().join(format!("expected/{name}.{ext}"))).unwrap();
        if text || expected.is_empty() {
            // an empty file: cargo-mutants printed nothing (a diff that leaves no mutant)
            assert_eq!(stdout, expected, "{name}");
        } else {
            let got: Value = serde_json::from_str(&same_marker(&stdout)).unwrap();
            let want: Value = serde_json::from_str(&expected).unwrap();
            let (got, want) = (got.as_array().unwrap(), want.as_array().unwrap());
            assert_eq!(got.len(), want.len(), "{name}: number of mutants");
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                assert_eq!(g, w, "{name}: mutant {i}");
            }
            // Same key order and layout, not just the same data.
            assert_eq!(same_marker(&stdout), expected, "{name}: raw output");
        }
    }
}

#[test]
fn fixture_covers_every_genre() {
    let (stdout, _, code) = run("kitchen_sink", &["--list".into(), "--json".into()]);
    assert_eq!(code, 0);
    let list: Vec<Value> = serde_json::from_str(&stdout).unwrap();
    for genre in [
        "FnValue",
        "BinaryOperator",
        "UnaryOperator",
        "MatchArm",
        "MatchArmGuard",
        "StructField",
    ] {
        assert!(
            list.iter().any(|m| m["genre"] == genre),
            "no {genre} mutant"
        );
    }
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        vec!["--list", "--shard", "2/2"],
        vec!["--list", "--shard", "1"],
        vec!["--list", "--shard", "0/0"],
        vec!["--list", "--shard", "a/b"],
        vec!["--list", "--in-diff"],
        vec!["--list", "-j", "2"],
        vec!["--bogus"],
    ] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        let (_, _, code) = run("kitchen_sink", &args);
        assert_eq!(code, 2, "{args:?}");
    }
}

#[test]
fn failures_other_than_usage_exit_1() {
    let (_, stderr, code) = run(
        "kitchen_sink",
        &["--list".into(), "--re".into(), "(".into()],
    );
    assert_eq!(code, 1, "{stderr}");
}

#[test]
fn in_diff_that_cannot_be_used_has_the_exit_codes_of_cargo_mutants() {
    let stale = fixtures().join("diffs/kitchen_sink_stale.diff");
    let (stdout, stderr, code) = run(
        "kitchen_sink",
        &[
            "--list".into(),
            "--in-diff".into(),
            stale.to_str().unwrap().into(),
        ],
    );
    assert_eq!((code, stdout.as_str()), (5, ""), "{stderr}");
    assert!(stderr.contains("src/ops.rs line 4"), "{stderr}");
    assert!(stderr.contains("out of date"), "{stderr}");
    let (stdout, stderr, code) = run(
        "kitchen_sink",
        &["--list".into(), "--in-diff".into(), "no-such.diff".into()],
    );
    assert_eq!((code, stdout.as_str()), (6, ""), "{stderr}");
    assert!(stderr.contains("Failed to read diff file"), "{stderr}");
}

#[test]
fn in_diff_reads_standard_input_for_a_dash() {
    use std::io::Write;
    let diff = std::fs::read(fixtures().join("diffs/kitchen_sink_edit.diff")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
        .args(["schemata", "--list", "--in-diff", "-"])
        .current_dir(fixtures().join("kitchen_sink"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&diff).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let expected =
        std::fs::read_to_string(fixtures().join("expected/kitchen_sink_in_diff_text.txt")).unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
}

#[test]
fn unknown_config_keys_are_refused_and_known_ones_warned_about() {
    let dir = std::env::temp_dir().join(format!("schemata-d-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("mutants.toml");
    let list = |text: &str| {
        std::fs::write(&config, text).unwrap();
        run(
            "kitchen_sink",
            &[
                "--list".into(),
                "--config".into(),
                config.to_str().unwrap().into(),
                "--file".into(),
                "src/ops.rs".into(),
            ],
        )
    };
    let (stdout, stderr, code) = list("exclude_re = [\"is_zero\"]\nbogus_key = 1\n");
    assert_eq!((code, stdout.as_str()), (2, ""), "{stderr}");
    assert!(stderr.contains("unknown key `bogus_key`"), "{stderr}");
    // keys cargo-mutants knows, which cargo-schemata does not use: a warning, and the rest is used
    let (stdout, stderr, code) = list("exclude_re = [\"is_zero\"]\ntimeout_multiplier = 3.0\n");
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("`timeout_multiplier` is not used"),
        "{stderr}"
    );
    assert!(stdout.contains("replace add"), "{stdout}");
    assert!(!stdout.contains("is_zero"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

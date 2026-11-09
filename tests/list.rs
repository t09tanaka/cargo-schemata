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
    assert!(cases.len() >= 10);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let text = case["text"].as_bool().unwrap_or(false);
        let mut args: Vec<String> = vec!["schemata".into(), "--list".into()];
        if !text {
            args.push("--json".into());
        }
        args.extend(
            case["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_owned()),
        );
        let (stdout, stderr, code) = run(case["dir"].as_str().unwrap(), &args);
        assert_eq!(code, 0, "{name}: {stderr}");
        let ext = if text { "txt" } else { "json" };
        let expected =
            std::fs::read_to_string(fixtures().join(format!("expected/{name}.{ext}"))).unwrap();
        if text {
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
        vec!["--list", "--in-diff", "x.diff"],
        vec!["--list", "--shard", "1/2"],
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

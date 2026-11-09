//! `--build-only` on a fixture crate: how mutants are classified, that the
//! embedded test binary reacts to `CARGO_SCHEMATA_MUTANT`, and that the source
//! tree is left alone.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_kit")
}

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

struct Built {
    dir: PathBuf,
    report: Value,
}

impl Drop for Built {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn build_only(dir: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
        .args(["schemata", "--build-only", "--json", "-d"])
        .arg(dir)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn built() -> Built {
    let dir = std::env::temp_dir().join(format!("schemata-b-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_dir(&fixture(), &dir);
    let report = build_only(&dir);
    Built { dir, report }
}

fn find<'a>(report: &'a Value, class: &str, name_part: &str) -> &'a Value {
    let hits: Vec<&Value> = report[class]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["name"].as_str().unwrap().contains(name_part))
        .collect();
    assert_eq!(hits.len(), 1, "{class} {name_part}: {hits:?}");
    hits[0]
}

fn run_behavior(bins: &Value, mutant: Option<u64>) -> bool {
    let bin = bins
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["target"] == "behavior")
        .expect("behavior test binary");
    let mut cmd = Command::new(bin["path"].as_str().unwrap());
    cmd.current_dir(bin["cwd"].as_str().unwrap())
        .env_remove("CARGO_SCHEMATA_MUTANT");
    for kv in bin["env"].as_array().unwrap() {
        cmd.env(kv[0].as_str().unwrap(), kv[1].as_str().unwrap());
    }
    if let Some(id) = mutant {
        cmd.env("CARGO_SCHEMATA_MUTANT", id.to_string());
    }
    cmd.output().unwrap().status.success()
}

#[test]
fn mutants_are_classified_and_embedded_ones_take_effect() {
    let b = built();
    let r = &b.report;
    let src_before = std::fs::read(fixture().join("src/lib.rs")).unwrap();
    assert_eq!(std::fs::read(b.dir.join("src/lib.rs")).unwrap(), src_before);

    // embedded: plain values, operators, nested operators, arms, guards, `?`
    for part in [
        "replace sum_text -> String with String::new()",
        "replace * with + in poly",
        "replace + with - in poly",
        "replace - with / in poly",
        "delete match arm 1 in name",
        "replace match guard x < 0 with true",
        "replace == with != in evens",
        "with Ok(0)",
        "replace * with / in double",
    ] {
        find(r, "embedded", part);
    }
    // the type changes (`P + P` is `P`, `P - P` is a number), so it is built on its own and works
    let swap = find(r, "single", "replace + with - in sum_text");
    assert!(swap["why"].as_str().unwrap().contains("E0308"));
    // `-> impl Iterator`: another concrete type than the body
    let it = find(r, "single", "with ::std::iter::empty()");
    assert!(it["why"].as_str().unwrap().contains("E0308"));
    // const context and struct field deletion are always built on their own
    let field = find(r, "single", "delete field a from struct Cfg");
    assert!(field["why"].as_str().unwrap().contains("StructField"));
    let consts: Vec<&Value> = r["single"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["why"].as_str().unwrap().contains("Const"))
        .collect();
    assert_eq!(consts.len(), 1, "{consts:?}");
    // do not compile in any form
    let times = find(r, "unviable", "replace + with * in sum_text");
    assert!(times["reason"].as_str().unwrap().contains("E0369"));
    for u in r["unviable"].as_array().unwrap() {
        if u["name"]
            .as_str()
            .unwrap()
            .contains("replace && with || in both")
        {
            assert!(u["reason"].as_str().unwrap().contains("let chain"));
        }
    }
    assert!(
        r["unviable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["reason"].as_str().unwrap().contains("E0080")),
        "const overflow is unviable"
    );
    let total = r["embedded"].as_array().unwrap().len()
        + r["single"].as_array().unwrap().len()
        + r["unviable"].as_array().unwrap().len();
    assert_eq!(total, 64);
    assert!(!r["stats"]["budget_exhausted"].as_bool().unwrap());

    // the embedded binaries: nothing selected passes, the selected mutant is caught
    let bins = &r["test_binaries"];
    assert!(run_behavior(bins, None));
    assert!(run_behavior(bins, Some(1_000_000)));
    for part in [
        "replace sum_text -> String with String::new()",
        "replace * with + in poly",
        "replace - with / in poly",
        "delete match arm 1 in name",
        "replace match guard x < 0 with true",
        "replace == with != in evens",
        "with Ok(0)",
        "replace * with / in double",
    ] {
        let id = find(r, "embedded", part)["id"].as_u64().unwrap();
        assert!(!run_behavior(bins, Some(id)), "{part} survived");
    }

    // single builds carry their own binaries with the mutation compiled in
    for part in [
        "delete field a from struct Cfg",
        "replace + with - in sum_text",
    ] {
        let m = find(r, "single", part);
        assert!(
            !run_behavior(&m["test_binaries"], None),
            "{part} should fail its tests"
        );
    }
    let it = find(r, "single", "with ::std::iter::empty()");
    assert!(!run_behavior(&it["test_binaries"], None));

    // a second run reuses the work directory and gives the same classification
    let again = build_only(&b.dir);
    for class in ["embedded", "single", "unviable"] {
        let ids = |v: &Value| -> Vec<u64> {
            v[class]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_u64().unwrap())
                .collect()
        };
        assert_eq!(ids(&again), ids(r), "{class}");
    }
}

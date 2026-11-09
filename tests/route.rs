//! Routing: each mutant runs only the tests that reach it (found by running
//! every test alone once), and ends when a test fails. The results have to be
//! the same as when every test runs for every mutant (`--no-routing`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/route_kit")
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

struct Kit {
    base: PathBuf,
    dir: PathBuf,
}

impl Kit {
    fn new(name: &str) -> Kit {
        let base = std::env::temp_dir().join(format!("schemata-r-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("kit");
        copy_dir(&fixture(), &dir);
        Kit { base, dir }
    }

    /// Run, and return the process output and the report.
    fn run(&self, out: &str, args: &[&str]) -> (Output, Report) {
        let out_dir = self.base.join(out);
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
            .args(["schemata", "-d"])
            .arg(&self.dir)
            .args(["-j", "4", "-o"])
            .arg(&out_dir)
            .args(args)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .output()
            .unwrap();
        let report =
            serde_json::from_slice(&std::fs::read(out_dir.join("report.json")).unwrap()).unwrap();
        (output, Report(report))
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

struct Report(Value);

struct Entry {
    status: String,
    reason: Option<String>,
    millis: u64,
}

impl Report {
    fn entries(&self) -> BTreeMap<String, Entry> {
        self.0["files"]["src/lib.rs"]["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["description"].as_str().unwrap().to_owned(),
                    Entry {
                        status: m["status"].as_str().unwrap().to_owned(),
                        reason: m["statusReason"].as_str().map(str::to_owned),
                        millis: m["duration"].as_u64().unwrap(),
                    },
                )
            })
            .collect()
    }

    fn statuses(&self) -> BTreeMap<String, String> {
        self.entries()
            .into_iter()
            .map(|(k, v)| (k, v.status))
            .collect()
    }
}

fn find<'a>(m: &'a BTreeMap<String, Entry>, part: &str) -> &'a Entry {
    let hits: Vec<_> = m.iter().filter(|(k, _)| k.contains(part)).collect();
    assert_eq!(
        hits.len(),
        1,
        "{part}: {:?}",
        hits.iter().map(|h| h.0).collect::<Vec<_>>()
    );
    hits[0].1
}

fn text(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

#[test]
fn routing_gives_the_results_of_a_full_run_and_is_faster() {
    let kit = Kit::new("all");
    let (out, routed) = kit.run("routed", &["--", "--lib"]);
    let stdout = text(&out.stdout);
    let entries = routed.entries();

    // lazy initialisation, other threads, tests that never see a mutant
    for part in [
        "replace base -> u32 with 0",
        "replace first -> u32 with 1",
        "replace + with - in threaded_sum",
        "replace * with / in threaded_sum",
        "replace add -> u32 with 0",
        "replace * with + in double",
        "replace gate -> bool with false",
    ] {
        assert_eq!(find(&entries, part).status, "Killed", "{part}\n{stdout}");
    }
    for part in ["replace > with >= in clamp", "replace > with == in clamp"] {
        let e = find(&entries, part);
        assert_eq!((e.status.as_str(), e.reason.as_deref()), ("Survived", None));
    }
    for part in [
        "replace never_called -> u32 with 0",
        "replace never_called -> u32 with 1",
    ] {
        let e = find(&entries, part);
        assert_eq!(e.status, "Survived", "{part}");
        assert_eq!(e.reason.as_deref(), Some("not reached by any test"));
        assert_eq!(e.millis, 0);
    }
    assert!(
        stdout
            .contains("replace never_called -> u32 with 0 ... missed (not reached by any test) in"),
        "{stdout}"
    );
    assert!(
        text(&out.stderr).contains("(missed: not reached by any test)"),
        "{}",
        text(&out.stderr)
    );
    assert!(stdout.contains("traced: 8 tests run alone"), "{stdout}");
    assert!(stdout.contains("mutants: run in "), "{stdout}");

    // a mutant is run against the tests that reach it only: the 3 s test is not one of them
    let clamp = find(&entries, "replace > with >= in clamp");
    assert!(clamp.millis < 1500, "{} ms", clamp.millis);
    // mutants that survive in a spawned thread and in a lazy initializer are run
    // against the tests that reach them alone; every test would take 3 s
    for part in [
        "replace > with >= in threaded_sum",
        "replace > with == in pad",
    ] {
        let e = find(&entries, part);
        assert_eq!(
            (e.status.as_str(), e.reason.as_deref()),
            ("Survived", None),
            "{part}"
        );
        assert!(e.millis < 1500, "{part}: {} ms", e.millis);
    }
    // ... and ends when a test fails, without waiting for the slow one
    let gate = find(&entries, "replace gate -> bool with false");
    assert!(gate.millis < 1500, "{} ms", gate.millis);

    // the same run without routing: same statuses, but it waits for the slow test
    let (out, full) = kit.run("full", &["--no-routing", "--", "--lib"]);
    let stdout = text(&out.stdout);
    assert!(!stdout.contains("traced:"), "{stdout}");
    assert_eq!(routed.statuses(), full.statuses());
    let slow = full.entries();
    assert!(find(&slow, "replace > with >= in clamp").millis >= 2500);
    assert!(find(&slow, "replace gate -> bool with false").millis >= 2500);
    assert_eq!(
        find(&slow, "replace never_called -> u32 with 0").reason,
        None
    );
}

#[test]
fn name_filters_and_skips_are_applied_when_routing() {
    let kit = Kit::new("filter");
    // (name, arguments, what each of `add`, `double` and `first` ends as)
    let cases: [(&str, Vec<&str>, [&str; 3]); 3] = [
        // a bare word is a name filter, as for `cargo test`
        (
            "only",
            vec!["--", "--lib", "alpha"],
            ["Killed", "Survived", "Survived"],
        ),
        (
            "skip",
            vec!["--", "--lib", "--", "--skip", "beta", "--skip", "gate_slow"],
            ["Killed", "Survived", "Killed"],
        ),
        (
            "exact",
            vec![
                "--",
                "--lib",
                "--",
                "--test-threads",
                "1",
                "--exact",
                "tests::alpha_add",
                "tests::gate_fast",
            ],
            ["Killed", "Survived", "Survived"],
        ),
    ];
    for (name, args, expected) in cases {
        let (out, routed) = kit.run(&format!("r-{name}"), &args);
        let mut full_args = vec!["--no-routing"];
        full_args.extend(&args);
        let (_, full) = kit.run(&format!("f-{name}"), &full_args);
        let stdout = text(&out.stdout);
        assert_eq!(routed.statuses(), full.statuses(), "{name}\n{stdout}");
        assert!(stdout.contains("traced: "), "{name}\n{stdout}");
        let e = routed.entries();
        for (part, want) in [
            ("replace add -> u32 with 0", expected[0]),
            ("replace double -> u32 with 0", expected[1]),
            ("replace first -> u32 with 0", expected[2]),
        ] {
            assert_eq!(find(&e, part).status, want, "{name}: {part}\n{stdout}");
        }
    }
}

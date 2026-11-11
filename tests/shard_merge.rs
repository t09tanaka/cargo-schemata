//! A run divided with `--shard k/n`, and put together again with
//! `cargo schemata merge`, on the fixture crate that has a mutant of every outcome.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_kit")
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

/// A copy of the fixture in a directory of its own.
struct Kit {
    base: PathBuf,
    dir: PathBuf,
}

impl Kit {
    fn new(name: &str) -> Kit {
        let base = std::env::temp_dir().join(format!("schemata-d-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("kit");
        copy_dir(&fixture(), &dir);
        Kit { base, dir }
    }

    fn out(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    /// Run with the output in `<base>/<out>`; the exit code and the stderr.
    fn run(&self, out: &str, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
            .args(["schemata", "-d"])
            .arg(&self.dir)
            .args(["-e", "main.rs", "-j", "2", "--timeout", "3", "-o"])
            .arg(self.out(out))
            .args(args)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .output()
            .unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn merge(&self, out: &str, parts: &[PathBuf], extra: &[&str]) -> (i32, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
            .args(["schemata", "merge"])
            .args(parts)
            .arg("-o")
            .arg(self.out(out))
            .args(extra)
            .output()
            .unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn report(&self, out: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.out(out).join("report.json")).unwrap()).unwrap()
    }

    fn list(&self, out: &str, file: &str) -> String {
        std::fs::read_to_string(self.out(out).join(file)).unwrap()
    }

    fn part(&self, out: &str) -> PathBuf {
        self.out(out).join("report.json")
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Every mutant of a report, in the order of the files and of each file.
fn mutants(report: &Value) -> Vec<&Value> {
    report["files"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|f| f["mutants"].as_array().unwrap())
        .collect()
}

/// A report as a run writes it, without the times, which differ from run to run.
fn without_durations(report: &Value) -> Value {
    let mut r = report.clone();
    for file in r["files"].as_object_mut().unwrap().values_mut() {
        for m in file["mutants"].as_array_mut().unwrap() {
            m.as_object_mut().unwrap().remove("duration");
        }
    }
    r
}

const LISTS: [&str; 4] = ["caught.txt", "missed.txt", "timeout.txt", "unviable.txt"];

#[test]
fn all_parts_merged_are_the_whole_run() {
    let kit = Kit::new("merge");
    let (code, err) = kit.run("whole", &[]);
    assert_eq!(code, 1, "{err}");
    let whole = kit.report("whole");
    let total = mutants(&whole).len();
    assert!(total >= 12, "{total}");
    assert!(whole.get("config").is_none());

    let n = 3;
    let mut evaluated = 0;
    for k in 0..n {
        let shard = format!("{k}/{n}");
        let (code, err) = kit.run(&format!("part{k}"), &["--shard", &shard]);
        let part = kit.report(&format!("part{k}"));
        // the report lists every mutant; the ones of the other parts are Pending
        assert_eq!(mutants(&part).len(), total, "{shard}");
        let all = mutants(&part);
        let own: Vec<&&Value> = all.iter().filter(|m| m["status"] != "Pending").collect();
        // round-robin is the default: mutant i goes to part i mod n
        let dealt: Vec<&str> = (k..total)
            .step_by(n)
            .map(|i| part["config"]["shard"]["order"][i].as_str().unwrap())
            .collect();
        // the report groups the mutants by file, `order` is the order of the list
        let mut own_ids: Vec<&str> = own.iter().map(|m| m["id"].as_str().unwrap()).collect();
        own_ids.sort_unstable();
        let mut dealt = dealt;
        dealt.sort_unstable();
        assert_eq!(own_ids, dealt, "{shard}");
        assert_eq!(part["config"]["shard"]["sharding"], "round-robin");
        assert_eq!(part["config"]["shard"]["index"], k);
        assert_eq!(part["config"]["shard"]["count"], n);
        assert_eq!(
            part["config"]["shard"]["order"].as_array().unwrap().len(),
            total
        );
        // a part judges its own mutants: the exit code and the lists
        let missed = own.iter().filter(|m| m["status"] == "Survived").count();
        assert_eq!(code, i32::from(missed > 0), "{shard}: {err}");
        assert!(
            err.contains(&format!("shard {shard}")) || missed == 0,
            "{err}"
        );
        let listed: usize = LISTS
            .iter()
            .map(|f| kit.list(&format!("part{k}"), f).lines().count())
            .sum();
        assert_eq!(listed, own.len(), "{shard}");
        evaluated += own.len();
    }
    assert_eq!(evaluated, total);

    // stale output of an earlier merge is removed whatever happens
    std::fs::create_dir_all(kit.out("merged")).unwrap();
    std::fs::write(kit.out("merged").join("missed.txt"), "stale\n").unwrap();
    let parts: Vec<PathBuf> = (0..n)
        .rev()
        .map(|k| kit.part(&format!("part{k}")))
        .collect();
    let (code, stdout, stderr) = kit.merge("merged", &parts, &[]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("surviving mutant(s)"), "{stderr}");
    assert!(stderr.contains("3 part(s) merged"), "{stderr}");
    assert_eq!(stdout, "");

    // the same report, the same statuses and the same lists as the whole run
    let merged = kit.report("merged");
    assert_eq!(without_durations(&merged), without_durations(&whole));
    let statuses = |r: &Value| -> Vec<String> {
        mutants(r)
            .iter()
            .map(|m| m["status"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(statuses(&merged), statuses(&whole));
    for f in LISTS {
        assert_eq!(kit.list("merged", f), kit.list("whole", f), "{f}");
    }
    for s in ["Killed", "Survived", "Timeout", "CompileError"] {
        assert!(statuses(&merged).iter().any(|x| x == s), "no {s}");
    }

    // --json prints the report
    let (_, stdout, _) = kit.merge("merged2", &parts, &["--json"]);
    assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), merged);
}

/// The ids of the mutants a part evaluated.
fn evaluated(report: &Value) -> Vec<String> {
    mutants(report)
        .iter()
        .filter(|m| m["status"] != "Pending")
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn slice_parts_merge_too_and_the_two_modes_are_not_mixed() {
    let kit = Kit::new("slice");
    let (code, err) = kit.run("whole", &[]);
    assert!(code <= 1, "{err}");
    let whole = kit.report("whole");
    let total = mutants(&whole).len();
    for k in 0..2 {
        let (code, err) = kit.run(
            &format!("s{k}"),
            &["--shard", &format!("{k}/2"), "--sharding", "slice"],
        );
        assert!(code <= 1, "{err}");
    }
    let (code, err) = kit.run("r1", &["--shard", "1/2", "--sharding", "round-robin"]);
    assert!(code <= 1, "{err}");
    let (code, err) = kit.run("r0", &["--shard", "0/2"]);
    assert!(code <= 1, "{err}");

    // slice: consecutive halves of the list
    let (s0, s1) = (kit.report("s0"), kit.report("s1"));
    assert_eq!(s0["config"]["shard"]["sharding"], "slice");
    let order: Vec<String> = s0["config"]["shard"]["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let half = total.div_ceil(2);
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    assert_eq!(sorted(evaluated(&s0)), sorted(order[..half].to_vec()));
    assert_eq!(sorted(evaluated(&s1)), sorted(order[half..].to_vec()));
    // round-robin is what a part gets without --sharding, and differs from a slice
    assert_eq!(
        kit.report("r1")["config"]["shard"]["sharding"],
        "round-robin"
    );
    assert_ne!(evaluated(&kit.report("r1")), evaluated(&s1));

    // each mode merges to the unsharded report
    let (code, _, err) = kit.merge("m_slice", &[kit.part("s0"), kit.part("s1")], &[]);
    assert!(code <= 1, "{err}");
    assert_eq!(
        without_durations(&kit.report("m_slice")),
        without_durations(&whole)
    );
    let (code, _, err) = kit.merge("m_rr", &[kit.part("r0"), kit.part("r1")], &[]);
    assert!(code <= 1, "{err}");
    assert_eq!(
        without_durations(&kit.report("m_rr")),
        without_durations(&whole)
    );

    // a report that names no mode (0.3.0) is a slice
    for k in 0..2 {
        let mut old = kit.report(&format!("s{k}"));
        old["config"]["shard"]
            .as_object_mut()
            .unwrap()
            .remove("sharding");
        std::fs::write(kit.out(&format!("old{k}.json")), old.to_string()).unwrap();
    }
    let (code, _, err) = kit.merge("m_old", &[kit.out("old0.json"), kit.out("old1.json")], &[]);
    assert!(code <= 1, "{err}");
    assert_eq!(
        without_durations(&kit.report("m_old")),
        without_durations(&whole)
    );

    // mixing the modes is refused, either way round, and nothing is written
    for (name, parts) in [
        ("mixed_a", [kit.part("s0"), kit.part("r1")]),
        ("mixed_b", [kit.part("r0"), kit.part("s1")]),
        ("mixed_old", [kit.out("old0.json"), kit.part("r1")]),
    ] {
        let (code, stdout, stderr) = kit.merge(name, &parts, &[]);
        assert_eq!(code, 1, "{name}: {stderr}");
        assert_eq!(stdout, "");
        assert!(stderr.contains("divided differently"), "{name}: {stderr}");
        assert!(stderr.contains("--sharding"), "{name}: {stderr}");
        assert!(stderr.contains("nothing was merged"), "{name}: {stderr}");
        assert!(!kit.out(name).join("report.json").exists(), "{name}");
    }
}

#[test]
fn the_output_may_be_the_directory_of_a_part() {
    let kit = Kit::new("inplace");
    for k in 0..2 {
        kit.run(&format!("part{k}"), &["--shard", &format!("{k}/2")]);
    }
    let parts = vec![kit.part("part0"), kit.part("part1")];
    // part0's report is read before its directory is cleared for the output
    let (code, _, stderr) = kit.merge("part0", &parts, &[]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("2 part(s) merged"), "{stderr}");
    let merged = kit.report("part0");
    assert!(merged.get("config").is_none(), "the merged report is whole");
}

#[test]
fn parts_that_are_not_all_the_parts_of_one_run_are_refused() {
    let kit = Kit::new("refuse");
    for k in 0..2 {
        let (code, err) = kit.run(&format!("p{k}"), &["--shard", &format!("{k}/2")]);
        assert!(code <= 1, "{err}");
    }
    let (code, err) = kit.run("whole", &[]);
    assert!(code <= 1, "{err}");
    let (p0, p1) = (kit.part("p0"), kit.part("p1"));
    let refused = |name: &str, parts: &[PathBuf], want: &str| {
        // an earlier result that is there must not stay
        std::fs::create_dir_all(kit.out(name)).unwrap();
        std::fs::write(kit.out(name).join("report.json"), "{}").unwrap();
        std::fs::write(kit.out(name).join("caught.txt"), "old\n").unwrap();
        let (code, stdout, stderr) = kit.merge(name, parts, &[]);
        assert_eq!(code, 1, "{name}: {stderr}");
        assert_eq!(stdout, "", "{name}");
        assert!(stderr.contains(want), "{name}: {stderr}");
        assert!(stderr.contains("nothing was merged"), "{name}: {stderr}");
        for f in [
            "report.json",
            "caught.txt",
            "missed.txt",
            "timeout.txt",
            "unviable.txt",
        ] {
            assert!(!kit.out(name).join(f).exists(), "{name}: {f} was left");
        }
    };
    refused("missing", std::slice::from_ref(&p0), "shard 1/2 is missing");
    refused(
        "missing_first",
        std::slice::from_ref(&p1),
        "shard 0/2 is missing",
    );
    refused(
        "twice",
        &[p0.clone(), p1.clone(), p0.clone()],
        "shard 0/2 is given more than once",
    );
    refused(
        "whole_as_part",
        &[kit.part("whole"), p1.clone()],
        "not the report of a part",
    );
    refused(
        "absent",
        &[p0.clone(), kit.out("nothing.json")],
        "cannot be read",
    );
    std::fs::write(kit.out("junk.json"), "not json").unwrap();
    refused("junk", &[p0.clone(), kit.out("junk.json")], "not a report");

    // a part of a run divided another way
    let (code, err) = kit.run("p_of_3", &["--shard", "1/3"]);
    assert!(code <= 1, "{err}");
    refused(
        "other_count",
        &[p0.clone(), kit.part("p_of_3")],
        "divided differently",
    );

    // a part of another run: a mutant that is not the same
    let mut other: Value = serde_json::from_slice(&std::fs::read(&p1).unwrap()).unwrap();
    let file = other["files"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    file["mutants"][0]["description"] = Value::String("replace x with y in z".into());
    std::fs::write(kit.out("other.json"), other.to_string()).unwrap();
    refused(
        "other_run",
        &[p0.clone(), kit.out("other.json")],
        "the mutants differ",
    );

    // a part that evaluated a mutant of another part
    let mut greedy: Value = serde_json::from_slice(&std::fs::read(&p1).unwrap()).unwrap();
    let pending = greedy["files"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .flat_map(|f| f["mutants"].as_array_mut().unwrap().iter_mut())
        .find(|m| m["status"] == "Pending")
        .unwrap();
    pending["status"] = Value::String("Killed".into());
    std::fs::write(kit.out("greedy.json"), greedy.to_string()).unwrap();
    refused(
        "greedy",
        &[p0.clone(), kit.out("greedy.json")],
        "evaluated but belongs to another part",
    );

    // and the right parts still merge
    let (code, _, stderr) = kit.merge("ok", &[p0, p1], &[]);
    assert!(code <= 1, "{stderr}");
    assert!(kit.out("ok").join("report.json").exists());
    let whole = kit.report("whole");
    assert_eq!(
        without_durations(&kit.report("ok")),
        without_durations(&whole)
    );
}

#[test]
fn merge_needs_parts() {
    let o = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
        .args(["schemata", "merge"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

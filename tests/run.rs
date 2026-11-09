//! A plain run on a fixture crate that has a mutant of every outcome: what is
//! caught, missed, timed out and unviable, the files and the exit code, the
//! processes left behind, the lock, and interruption.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

/// A copy of the fixture in a directory of its own, and a file for the pids
/// of the processes the tests start.
struct Kit {
    dir: PathBuf,
    out: PathBuf,
    pids: PathBuf,
}

impl Kit {
    fn new(name: &str) -> Kit {
        let base = std::env::temp_dir().join(format!("schemata-c-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("kit");
        copy_dir(&fixture(), &dir);
        Kit {
            out: base.join("out"),
            pids: base.join("pids.txt"),
            dir,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"));
        cmd.args(["schemata", "-d"])
            .arg(&self.dir)
            .args(["-e", "main.rs", "-o"])
            .arg(&self.out)
            .args(args)
            .env("RUN_KIT_PIDS", &self.pids)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS");
        cmd
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.out.join("report.json")).unwrap()).unwrap()
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.out.join(file))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Every process the tests started is gone.
    fn assert_no_children_left(&self) {
        let pids: Vec<i32> = std::fs::read_to_string(&self.pids)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect();
        for pid in pids {
            let deadline = Instant::now() + Duration::from_secs(5);
            // SAFETY: signal 0 only tests whether the process exists.
            while unsafe { libc::kill(pid, 0) } == 0 {
                assert!(Instant::now() < deadline, "process {pid} is still alive");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        if let Some(base) = self.dir.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn status_of(report: &Value, name_part: &str) -> String {
    let hits: Vec<&Value> = report["files"]["src/lib.rs"]["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["description"].as_str().unwrap().contains(name_part))
        .collect();
    assert_eq!(hits.len(), 1, "{name_part}: {hits:?}");
    hits[0]["status"].as_str().unwrap().to_owned()
}

fn wait(mut child: Child, within: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("the run did not end within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn every_outcome_is_reported_and_one_run_at_a_time() {
    let kit = Kit::new("all");
    std::fs::create_dir_all(&kit.out).unwrap();
    std::fs::write(kit.out.join("missed.txt"), "stale\n").unwrap();
    std::fs::write(kit.out.join("mine.txt"), "keep\n").unwrap();
    let stdout = std::fs::File::create(kit.out.join("../stdout.txt")).unwrap();
    let stderr = std::fs::File::create(kit.out.join("../stderr.txt")).unwrap();
    let first = kit
        .command(&["-j", "2", "--timeout", "3"])
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();

    // a second run in the same package ends at once and changes nothing
    let lock = kit.dir.join("target/schemata/lock");
    let deadline = Instant::now() + Duration::from_secs(120);
    while std::fs::read_to_string(&lock)
        .unwrap_or_default()
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the first run never took the lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let second = kit
        .command(&["-j", "2", "--timeout", "3"])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    let msg = String::from_utf8_lossy(&second.stderr);
    assert!(
        msg.contains("another cargo schemata run is at work"),
        "{msg}"
    );
    assert!(msg.contains("pid "), "{msg}");
    assert_eq!(second.stdout, b"");

    let status = wait(first, Duration::from_secs(300));
    assert_eq!(status.code(), Some(1));

    // the files
    let report = kit.report();
    assert_eq!(report["schemaVersion"], "2");
    assert_eq!(report["framework"]["name"], "cargo-schemata");
    assert_eq!(report["files"]["src/lib.rs"]["language"], "rust");
    assert!(
        report["files"]["src/lib.rs"]["source"]
            .as_str()
            .unwrap()
            .contains("pub fn add")
    );
    for (part, want) in [
        ("replace add -> u32 with 0", "Killed"),
        ("replace + with - in add", "Killed"),
        ("replace * with + in twice", "Survived"),
        ("replace * with / in twice", "Killed"),
        ("replace += with *= in count_to", "Timeout"),
        ("replace += with -= in count_to", "Killed"),
        ("replace + with * in join", "CompileError"),
        // built on their own: the one the tests never look at is missed, the one
        // that only the binary of the same build gives away is caught
        ("replace unchecked -> u32 with 0", "Survived"),
        ("replace unchecked -> u32 with 1", "Survived"),
        ("replace limit -> u32 with 0", "Killed"),
        ("replace limit -> u32 with 1", "Killed"),
    ] {
        assert_eq!(status_of(&report, part), want, "{part}");
    }
    let timeout = report["files"]["src/lib.rs"]["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["status"] == "Timeout")
        .unwrap();
    assert_eq!(timeout["statusReason"], "did not end within 3s");
    let compile = report["files"]["src/lib.rs"]["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["status"] == "CompileError")
        .unwrap();
    assert!(compile["statusReason"].as_str().unwrap().contains("E0369"));
    assert_eq!(timeout["location"]["start"]["line"], 17);

    assert_eq!(
        kit.lines("timeout.txt"),
        ["src/lib.rs:17:11: replace += with *= in count_to"]
    );
    assert_eq!(
        kit.lines("missed.txt"),
        [
            "src/lib.rs:10:7: replace * with + in twice",
            "src/lib.rs:29:5: replace unchecked -> u32 with 0",
            "src/lib.rs:29:5: replace unchecked -> u32 with 1",
            "src/lib.rs:39:5: replace leave_a_child_behind with ()",
        ]
    );
    assert_eq!(kit.lines("unviable.txt").len(), 2);
    assert_eq!(kit.lines("caught.txt").len(), 17);
    assert_eq!(
        std::fs::read_to_string(kit.out.join("mine.txt")).unwrap(),
        "keep\n"
    );

    // what is said
    let stdout = std::fs::read_to_string(kit.out.join("../stdout.txt")).unwrap();
    let stderr = std::fs::read_to_string(kit.out.join("../stderr.txt")).unwrap();
    assert!(
        stdout.contains("replace add -> u32 with 0 ... caught in"),
        "{stdout}"
    );
    assert!(
        stdout.contains("replace += with *= in count_to ... timeout in"),
        "{stdout}"
    );
    assert!(
        stdout.contains("did not end within 3s and are run again alone"),
        "{stdout}"
    );
    assert!(!stdout.contains("FAILED"));
    assert!(
        stderr.contains("cargo schemata: 4 surviving mutant(s)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("src/lib.rs:10:7: replace * with + in twice (missed)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("cargo schemata: FAILED (17 caught, 4 missed, 1 timeout, 2 unviable in "),
        "{stderr}"
    );

    // the timeout ended the process group, and every test run ended its leftovers
    assert!(std::fs::read_to_string(&kit.pids).unwrap().lines().count() >= 2);
    kit.assert_no_children_left();
}

#[test]
fn no_survivor_exits_zero_and_json_is_the_only_thing_on_stdout() {
    let kit = Kit::new("ok");
    let out = kit.command(&["--json", "--re", "add"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed, kit.report());
    assert!(
        stderr.contains("cargo schemata: OK (4 caught, 0 missed, 0 timeout, 0 unviable in "),
        "{stderr}"
    );
    assert_eq!(kit.lines("caught.txt").len(), 4);
    assert_eq!(kit.lines("missed.txt"), Vec::<String>::new());
    kit.assert_no_children_left();

    // without --json the progress is on stdout
    let out = kit.command(&["--re", "add", "-j", "1"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cargo schemata: OK ("), "{stdout}");
}

#[test]
fn tests_that_fail_without_a_mutant_stop_the_run() {
    let kit = Kit::new("base");
    std::fs::write(
        kit.dir.join("tests/broken.rs"),
        "#[test]\nfn fails() {\n    println!(\"the marker line\");\n    panic!(\"broken\");\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(&kit.out).unwrap();
    std::fs::write(kit.out.join("report.json"), "{}").unwrap();
    let out = kit.command(&["--re", "add"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fail without any mutant"), "{stderr}");
    assert!(stderr.contains("the marker line"), "{stderr}");
    assert!(
        !kit.out.join("report.json").exists(),
        "an old report must not stay"
    );
}

#[test]
fn an_interrupted_run_stops_its_children_and_writes_nothing() {
    let kit = Kit::new("int");
    let mut child = kit
        .command(&["-j", "1", "--timeout", "3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            lines.read_line(&mut line).unwrap() > 0,
            "the run ended early"
        );
        if line.contains("... caught in") {
            break;
        }
    }
    // SAFETY: plain signal to our own child.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = wait(child, Duration::from_secs(60));
    assert_eq!(status.code(), Some(143));
    assert!(!kit.out.join("report.json").exists());
    kit.assert_no_children_left();
}

#[test]
fn wrong_usage_exits_with_two() {
    for args in [
        &["-j", "0"][..],
        &["--timeout", "0"],
        &["--in-diff=x"],
        &["--list", "-j", "2"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-schemata"))
            .arg("schemata")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

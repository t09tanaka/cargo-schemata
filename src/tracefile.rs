//! The baseline and the trace of a run in a file, so that the parts of a
//! divided run do not each make them again.
//!
//! `--trace-only` builds every mutant, runs the baseline and the trace and
//! writes `trace.json`; `--reuse-trace <file>` reads it instead of running the
//! two stages. What the file holds is only true for the inputs it was made
//! from, so it carries an [`Identity`] of them, and a run whose own identity
//! differs in anything refuses the file: it does not trace again and does not
//! go on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mutant::Mutant;
use crate::prepare::TestBinary;
use crate::route::Routing;
use crate::sync::left_out;

/// The name of the file in the `--output` directory.
pub const FILE_NAME: &str = "trace.json";
/// The version of the file's layout.
const FORMAT: u32 = 1;
/// How many differing files a refusal names.
const NAMED_FILES: usize = 8;

/// Everything the baseline and the trace depend on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    format: u32,
    /// The version of cargo-schemata: the rewriting and the routing rules.
    version: String,
    /// `rustc -vV`, run in the workspace (a toolchain file applies).
    rustc: String,
    /// `cargo -V`.
    cargo: String,
    /// The rustflags of the build, as they are passed on.
    rustflags: String,
    /// What follows `--` for cargo: targets, features, profile.
    cargo_args: Vec<String>,
    /// What follows `--` for the test binaries: filters and libtest options.
    test_args: Vec<String>,
    /// `--timeout` in microseconds: the limit of the baseline and of a test alone.
    timeout: Option<u64>,
    /// The packages that are mutated.
    packages: Vec<String>,
    /// How many mutants the whole run has.
    mutants: usize,
    /// A digest of their ids and names, in the order of the list.
    mutants_digest: String,
    /// A digest of every file of the tree that is copied for the build, by its
    /// path from the workspace root.
    sources: BTreeMap<String, String>,
}

/// What an [`Identity`] is made from.
pub struct Inputs<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// Paths that are not part of the tree: the target directory, the output
    /// directory of this run and the trace file that is read.
    pub exclude: &'a [PathBuf],
    pub packages: Vec<String>,
    /// Every mutant of the run (of all parts).
    pub mutants: &'a [Mutant],
    pub cargo_args: &'a [String],
    pub test_args: &'a [String],
    pub timeout: Option<Duration>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn tool_version(program: &str, arg: &str, dir: &Path) -> Result<String> {
    let out = Command::new(program)
        .arg(arg)
        .current_dir(dir)
        .output()
        .with_context(|| format!("run {program} {arg}"))?;
    if !out.status.success() {
        bail!(
            "{program} {arg} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A digest of the ids and names of the mutants, in order.
fn mutants_digest(mutants: &[Mutant]) -> String {
    let mut h = Sha256::new();
    for m in mutants {
        h.update(format!("{}\t{}\t{}\n", m.id, m.package, m.name(true)));
    }
    hex(&h.finalize())
}

/// A digest of every file and symbolic link below `root` that the build copies
/// ([`left_out`] says which are not), without the `exclude` paths. Directories
/// are not entries: an empty one changes nothing.
pub fn digest_tree(root: &Path, exclude: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve {}", root.display()))?;
    // A path that does not exist is not in the tree anyway.
    let exclude: Vec<PathBuf> = exclude
        .iter()
        .filter_map(|e| e.canonicalize().ok())
        .collect();
    let mut out = BTreeMap::new();
    digest_dir(&root, Path::new(""), &exclude, &mut out)?;
    Ok(out)
}

fn digest_dir(
    root: &Path,
    rel: &Path,
    exclude: &[PathBuf],
    out: &mut BTreeMap<String, String>,
) -> Result<()> {
    let dir = root.join(rel);
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        if left_out(rel, &name, &path, exclude) {
            continue;
        }
        let rel_path = rel.join(&name);
        let meta = std::fs::symlink_metadata(&path)?;
        let key = rel_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if meta.is_dir() {
            digest_dir(root, &rel_path, exclude, out)?;
        } else if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path)?;
            let mut h = Sha256::new();
            h.update(b"link\0");
            h.update(target.as_os_str().as_encoded_bytes());
            out.insert(key, hex(&h.finalize()));
        } else {
            let mut h = Sha256::new();
            h.update(b"file\0");
            let mut file =
                std::fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
            std::io::copy(&mut file, &mut h).with_context(|| format!("read {}", path.display()))?;
            out.insert(key, hex(&h.finalize()));
        }
    }
    Ok(())
}

impl Identity {
    pub fn of(inputs: &Inputs) -> Result<Identity> {
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
        Ok(Identity {
            format: FORMAT,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            rustc: tool_version(&rustc, "-vV", inputs.root)?,
            cargo: tool_version(&crate::workspace::cargo_bin(), "-V", inputs.root)?,
            rustflags: crate::prepare::encoded_rustflags().replace('\x1f', " "),
            cargo_args: inputs.cargo_args.to_vec(),
            test_args: inputs.test_args.to_vec(),
            timeout: inputs
                .timeout
                .map(|t| u64::try_from(t.as_micros()).unwrap_or(u64::MAX)),
            packages: inputs.packages.clone(),
            mutants: inputs.mutants.len(),
            mutants_digest: mutants_digest(inputs.mutants),
            sources: digest_tree(inputs.root, inputs.exclude)?,
        })
    }

    /// What differs between the identity in a file (`self`) and the one of
    /// this run, one line each. Empty when the file was made for this run.
    pub fn differences(&self, here: &Identity) -> Vec<String> {
        let mut out = Vec::new();
        if self.format != here.format {
            // nothing else can be compared
            return vec![format!(
                "the layout of the file is version {}, and this cargo-schemata reads version {}",
                self.format, here.format
            )];
        }
        if self.version != here.version {
            out.push(format!(
                "cargo-schemata {} wrote the file, and this is cargo-schemata {}",
                self.version, here.version
            ));
        }
        let one_line = |s: &str| s.lines().collect::<Vec<_>>().join("; ");
        if self.rustc != here.rustc {
            out.push(format!(
                "the toolchain differs: `{}` in the file, `{}` here",
                one_line(&self.rustc),
                one_line(&here.rustc)
            ));
        }
        if self.cargo != here.cargo {
            out.push(format!(
                "cargo differs: `{}` in the file, `{}` here",
                self.cargo, here.cargo
            ));
        }
        if self.rustflags != here.rustflags {
            out.push(format!(
                "the rustflags differ: `{}` in the file, `{}` here",
                self.rustflags, here.rustflags
            ));
        }
        if self.cargo_args != here.cargo_args {
            out.push(format!(
                "the arguments for cargo (after `--`) differ: {:?} in the file, {:?} here",
                self.cargo_args, here.cargo_args
            ));
        }
        if self.test_args != here.test_args {
            out.push(format!(
                "the arguments for the test binaries (after `--`) differ: {:?} in the file, {:?} here",
                self.test_args, here.test_args
            ));
        }
        if self.timeout != here.timeout {
            let show = |t: Option<u64>| {
                t.map_or_else(
                    || "none".to_owned(),
                    |us| format!("{}s", Duration::from_micros(us).as_secs_f64()),
                )
            };
            out.push(format!(
                "--timeout differs: {} in the file, {} here",
                show(self.timeout),
                show(here.timeout)
            ));
        }
        if self.packages != here.packages {
            out.push(format!(
                "the packages differ: {:?} in the file, {:?} here",
                self.packages, here.packages
            ));
        }
        if self.mutants != here.mutants || self.mutants_digest != here.mutants_digest {
            out.push(format!(
                "the mutants differ (the filters, --in-diff or the configuration file): {} in the file, {} here",
                self.mutants, here.mutants
            ));
        }
        if self.sources != here.sources {
            let mut names: Vec<String> = Vec::new();
            for (path, digest) in &self.sources {
                match here.sources.get(path) {
                    Some(d) if d == digest => {}
                    Some(_) => names.push(format!("{path} (changed)")),
                    None => names.push(format!("{path} (only when the file was written)")),
                }
            }
            for path in here.sources.keys() {
                if !self.sources.contains_key(path) {
                    names.push(format!("{path} (only here)"));
                }
            }
            names.sort();
            let more = names.len().saturating_sub(NAMED_FILES);
            names.truncate(NAMED_FILES);
            let mut line = format!("the source tree differs: {}", names.join(", "));
            if more > 0 {
                line.push_str(&format!(" and {more} more"));
            }
            out.push(line);
        }
        out
    }
}

/// Which test binary: the same on every machine, unlike its path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BinaryName {
    package: String,
    target: String,
    kind: String,
}

impl BinaryName {
    pub fn of(bin: &TestBinary) -> BinaryName {
        BinaryName {
            package: bin.package.clone(),
            target: bin.target.clone(),
            kind: bin.kind.clone(),
        }
    }
}

impl std::fmt::Display for BinaryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}, {})", self.target, self.package, self.kind)
    }
}

/// How the trace ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Every test was run alone.
    Routed(Routing),
    /// No test binary prints what libtest prints, so no test can be run alone.
    NoLibtest,
    /// The baseline recorded no mutant.
    NothingRecorded,
    /// Tracing took longer than it may.
    GaveUp,
}

/// The two stages: the baseline and the trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stages {
    /// The test binaries, in the order the routing numbers them.
    pub binaries: Vec<BinaryName>,
    /// What the tests took without a mutant: the time limit comes from it.
    pub baseline: Duration,
    /// The mutants that were in the build the trace was made on. A mutant
    /// that is not among them was never recorded.
    pub embedded: BTreeSet<usize>,
    pub trace: Outcome,
}

impl Stages {
    /// The baseline and the routing for a build of this run: its test
    /// `binaries` and the mutants `embedded` in it. `None` for the routing is
    /// a run of every test for each mutant.
    ///
    /// The binaries have to be the ones of the file, in any order (cargo names
    /// them as they are finished). Anything else is refused.
    pub fn for_build(
        self,
        binaries: &[TestBinary],
        embedded: &BTreeSet<usize>,
    ) -> Result<(Duration, Option<Routing>)> {
        let here: Vec<BinaryName> = binaries.iter().map(BinaryName::of).collect();
        let list = |names: &[BinaryName]| {
            names
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut to = Vec::new();
        for name in &self.binaries {
            let mut places = here.iter().enumerate().filter(|(_, h)| *h == name);
            match (places.next(), places.next()) {
                (Some((at, _)), None) => to.push(at),
                _ => bail!(
                    "the build has other test binaries than the file: [{}] in the file, [{}] here",
                    list(&self.binaries),
                    list(&here)
                ),
            }
        }
        let distinct: BTreeSet<usize> = to.iter().copied().collect();
        if distinct.len() != here.len() || to.len() != here.len() {
            bail!(
                "the build has other test binaries than the file: [{}] in the file, [{}] here",
                list(&self.binaries),
                list(&here)
            );
        }
        let routing = match self.trace {
            Outcome::Routed(routing) => {
                routing
                    .check(here.len())
                    .context("the routing in the file is damaged")?;
                routing.reordered(&to).for_build(embedded, &self.embedded)
            }
            Outcome::NoLibtest | Outcome::NothingRecorded | Outcome::GaveUp => None,
        };
        Ok((self.baseline, routing))
    }
}

/// What `--trace-only` writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFile {
    pub identity: Identity,
    /// `None` when the run has no mutants: nothing was built or run.
    pub stages: Option<Stages>,
}

impl TraceFile {
    /// Write `trace.json` into `dir`, whole or not at all. The path.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join(FILE_NAME);
        let partial = dir.join(format!("{FILE_NAME}.{}.partial", std::process::id()));
        let text = serde_json::to_string(self).expect("serialize the trace") + "\n";
        std::fs::write(&partial, text).with_context(|| format!("write {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }

    /// Read the file, and refuse it unless it was made for the run `here`.
    pub fn read_for(path: &Path, here: &Identity) -> Result<TraceFile> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("--reuse-trace: read {}", path.display()))?;
        let file: TraceFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(e) => {
                // A file of another layout says so, when it can still be read that far.
                #[derive(Deserialize)]
                struct Head {
                    identity: HeadIdentity,
                }
                #[derive(Deserialize)]
                struct HeadIdentity {
                    format: u32,
                }
                match serde_json::from_str::<Head>(&text) {
                    Ok(head) if head.identity.format != FORMAT => bail!(
                        "--reuse-trace {}: the layout of the file is version {}, and this cargo-schemata reads version {FORMAT}; nothing was run",
                        path.display(),
                        head.identity.format
                    ),
                    _ => bail!(
                        "--reuse-trace {}: not a file that `--trace-only` wrote ({e}); nothing was run",
                        path.display()
                    ),
                }
            }
        };
        let differences = file.identity.differences(here);
        if !differences.is_empty() {
            bail!(
                "--reuse-trace {}: the file was not made for this run; nothing was run (make it again with --trace-only, with the same source tree, toolchain and options)\n  - {}",
                path.display(),
                differences.join("\n  - ")
            );
        }
        Ok(file)
    }
}

/// Remove the file of an earlier `--trace-only` in `dir`, so that a run that
/// fails does not leave one that looks like its own.
pub fn clear(dir: &Path) -> Result<()> {
    let path = dir.join(FILE_NAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn identity() -> Identity {
        Identity {
            format: FORMAT,
            version: "0.4.0".into(),
            rustc: "rustc 1.89.0 (abc 2025-08-04)\nhost: x86_64-unknown-linux-gnu".into(),
            cargo: "cargo 1.89.0 (def 2025-06-23)".into(),
            rustflags: "--cap-lints=warn".into(),
            cargo_args: v(&["--lib"]),
            test_args: v(&["--test-threads", "1"]),
            timeout: Some(3_000_000),
            packages: v(&["kit"]),
            mutants: 2,
            mutants_digest: "aa".into(),
            sources: BTreeMap::from([
                ("Cargo.lock".to_owned(), "01".to_owned()),
                ("src/lib.rs".to_owned(), "02".to_owned()),
            ]),
        }
    }

    fn binary(package: &str, target: &str, kind: &str) -> TestBinary {
        TestBinary {
            path: format!("/machine/local/{target}-{kind}"),
            cwd: "/".into(),
            env: Vec::new(),
            package: package.into(),
            target: target.into(),
            kind: kind.into(),
        }
    }

    fn stages(binaries: &[TestBinary]) -> Stages {
        Stages {
            binaries: binaries.iter().map(BinaryName::of).collect(),
            baseline: Duration::new(7, 250_000_001),
            embedded: [1, 2, 3].into(),
            trace: Outcome::Routed(Routing::sample(binaries.len())),
        }
    }

    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("schemata-tracefile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Each input on its own: a file that differs in it alone is refused, and
    /// the refusal says what differs.
    #[test]
    fn a_file_made_for_any_other_input_is_refused() {
        let file = identity();
        assert_eq!(file.differences(&identity()), Vec::<String>::new());
        type Change = fn(&mut Identity);
        let cases: [(&str, Change); 13] = [
            ("layout of the file", |i| i.format += 1),
            ("cargo-schemata 0.4.0 wrote the file", |i| {
                i.version = "0.4.1".into()
            }),
            ("the toolchain differs", |i| {
                i.rustc = i.rustc.replace("1.89.0", "1.90.0")
            }),
            ("the toolchain differs", |i| {
                i.rustc = i.rustc.replace("x86_64", "aarch64")
            }),
            ("cargo differs", |i| i.cargo.push('x')),
            ("the rustflags differ", |i| i.rustflags.push_str(" -g")),
            ("the arguments for cargo", |i| {
                i.cargo_args = vec!["--features".into(), "x".into()]
            }),
            ("the arguments for the test binaries", |i| {
                i.test_args.clear()
            }),
            ("--timeout differs: 3s in the file, none here", |i| {
                i.timeout = None
            }),
            ("the packages differ", |i| i.packages.push("other".into())),
            ("the mutants differ", |i| i.mutants_digest = "bb".into()),
            ("the mutants differ", |i| i.mutants = 3),
            ("src/lib.rs (changed)", |i| {
                i.sources.insert("src/lib.rs".into(), "03".into());
            }),
        ];
        let d = dir("refused");
        let written = TraceFile {
            identity: file.clone(),
            stages: None,
        }
        .write(&d)
        .unwrap();
        for (says, change) in cases {
            let mut here = identity();
            change(&mut here);
            let differences = file.differences(&here);
            assert_eq!(differences.len(), 1, "{says}: {differences:?}");
            assert!(differences[0].contains(says), "{says}: {differences:?}");
            let err = format!("{:#}", TraceFile::read_for(&written, &here).unwrap_err());
            assert!(err.contains("nothing was run"), "{err}");
            assert!(err.contains(says), "{says}: {err}");
        }
        assert!(TraceFile::read_for(&written, &file).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_files_that_differ_are_named() {
        let file = identity();
        let mut here = identity();
        here.sources.remove("Cargo.lock");
        here.sources
            .insert("parts/0/report.json".into(), "09".into());
        here.sources.insert("src/lib.rs".into(), "03".into());
        assert_eq!(
            file.differences(&here),
            vec![
                "the source tree differs: Cargo.lock (only when the file was written), parts/0/report.json (only here), src/lib.rs (changed)"
            ]
        );
        for n in 0..20 {
            here.sources.insert(format!("new/{n:02}.rs"), "00".into());
        }
        let line = &file.differences(&here)[0];
        assert!(line.ends_with(" and 15 more"), "{line}");
        // everything that differs is said, not the first thing alone
        here.version = "9.9.9".into();
        here.timeout = Some(1);
        assert_eq!(file.differences(&here).len(), 3);
    }

    #[test]
    fn a_file_is_read_back_as_it_was_written() {
        let d = dir("round-trip");
        let bins = [binary("kit", "kit", "lib"), binary("kit", "cli", "test")];
        for stages in [
            None,
            Some(stages(&bins)),
            Some(Stages {
                trace: Outcome::GaveUp,
                ..stages(&bins)
            }),
            Some(Stages {
                trace: Outcome::NoLibtest,
                ..stages(&bins)
            }),
            Some(Stages {
                trace: Outcome::NothingRecorded,
                ..stages(&bins)
            }),
        ] {
            let file = TraceFile {
                identity: identity(),
                stages,
            };
            let path = file.write(&d).unwrap();
            assert_eq!(path, d.join(FILE_NAME));
            let first = std::fs::read_to_string(&path).unwrap();
            let back = TraceFile::read_for(&path, &identity()).unwrap();
            assert_eq!(back, file);
            // written again it is the same text, byte for byte
            back.write(&d).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        }
        // nothing else is left in the directory
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);

        clear(&d).unwrap();
        assert!(!d.join(FILE_NAME).exists());
        clear(&d).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn what_is_not_such_a_file_is_refused() {
        let d = dir("not-a-file");
        let path = d.join("x.json");
        let refusal = |text: &str| {
            std::fs::write(&path, text).unwrap();
            format!("{:#}", TraceFile::read_for(&path, &identity()).unwrap_err())
        };
        assert!(refusal("").contains("not a file that `--trace-only` wrote"));
        assert!(refusal("{\"files\": {}}").contains("not a file that `--trace-only` wrote"));
        // a later layout that this version cannot read says which it is
        let later = refusal("{\"identity\": {\"format\": 2, \"new\": true}, \"stages\": 1}");
        assert!(later.contains("version 2"), "{later}");
        assert!(later.contains("nothing was run"), "{later}");
        let missing = TraceFile::read_for(&d.join("none.json"), &identity()).unwrap_err();
        assert!(format!("{missing:#}").contains("none.json"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The limit of a mutant is made from the baseline in the file by the rule
    /// of a run that measured it itself, and the baseline is exact.
    #[test]
    fn the_baseline_of_the_file_sets_the_same_limit() {
        let bins = [binary("kit", "kit", "lib")];
        let embedded: BTreeSet<usize> = [1, 2, 3].into();
        let text = serde_json::to_string(&stages(&bins)).unwrap();
        let read: Stages = serde_json::from_str(&text).unwrap();
        let (baseline, routing) = read.for_build(&bins, &embedded).unwrap();
        assert_eq!(baseline, Duration::new(7, 250_000_001));
        assert!(routing.is_some());
        assert_eq!(
            crate::exec::mutant_timeout(baseline, None),
            Duration::from_secs(37)
        );
        assert_eq!(
            crate::exec::mutant_timeout(baseline, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn the_binaries_are_matched_by_name_in_any_order() {
        // a library and a program of the same name are two binaries
        let traced = [
            binary("kit", "kit", "lib"),
            binary("kit", "kit", "bin"),
            binary("kit", "cli", "test"),
        ];
        let embedded: BTreeSet<usize> = [1, 2, 3].into();
        let (_, same) = stages(&traced).for_build(&traced, &embedded).unwrap();
        assert_eq!(same, Some(Routing::sample(3)));

        // this build finished them in another order: the routing follows
        let here = [traced[2].clone(), traced[0].clone(), traced[1].clone()];
        let (_, moved) = stages(&traced).for_build(&here, &embedded).unwrap();
        assert_eq!(moved, Some(Routing::sample(3).reordered(&[1, 2, 0])));
        assert_ne!(moved, same);

        // other binaries: one missing, one more, another one, one twice
        let refused = |here: &[TestBinary]| {
            let err = stages(&traced).for_build(here, &embedded).unwrap_err();
            assert!(
                format!("{err:#}").contains("other test binaries than the file"),
                "{err:#}"
            );
        };
        refused(&traced[..2]);
        let mut more = traced.to_vec();
        more.push(binary("kit", "extra", "test"));
        refused(&more);
        refused(&[
            traced[0].clone(),
            traced[1].clone(),
            binary("kit", "cli", "bench"),
        ]);
        refused(&[traced[0].clone(), traced[0].clone(), traced[2].clone()]);

        // a trace without a routing gives none, whatever the build
        for trace in [
            Outcome::GaveUp,
            Outcome::NoLibtest,
            Outcome::NothingRecorded,
        ] {
            let s = Stages {
                trace,
                ..stages(&traced)
            };
            assert_eq!(s.for_build(&here, &embedded).unwrap().1, None);
        }
        // a routing that names what is not there is refused
        let s = Stages {
            trace: Outcome::Routed(Routing::sample(2)),
            ..stages(&traced)
        };
        let err = s.for_build(&traced, &embedded).unwrap_err();
        assert!(format!("{err:#}").contains("damaged"), "{err:#}");
    }

    #[test]
    fn the_digest_is_of_the_files_the_build_copies() {
        let d = dir("digest");
        let root = d.join("tree");
        for sub in [
            "src",
            ".git",
            "target/debug",
            "out",
            "empty",
            "prep",
            ".cargo",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let write = |rel: &str, text: &str| std::fs::write(root.join(rel), text).unwrap();
        write("Cargo.lock", "lock");
        write("src/lib.rs", "pub fn f() {}\n");
        write(".cargo/config.toml", "");
        write(".git/HEAD", "ref");
        write("target/debug/x", "built");
        write("out/report.json", "{}");
        write("prep/trace.json", "{}");
        std::os::unix::fs::symlink("src/lib.rs", root.join("link.rs")).unwrap();

        let exclude = [root.join("out"), root.join("prep/trace.json")];
        let digest = digest_tree(&root, &exclude).unwrap();
        // not `.git`, not `target`, not what is excluded, and no directory
        assert_eq!(
            digest.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![".cargo/config.toml", "Cargo.lock", "link.rs", "src/lib.rs"]
        );
        assert_eq!(digest["src/lib.rs"].len(), 64);
        // a link is not the file it names
        assert_ne!(digest["link.rs"], digest["src/lib.rs"]);
        // an excluded path that is not there, and one given through a link
        // to the tree, change nothing
        std::os::unix::fs::symlink(&root, d.join("alias")).unwrap();
        let other = [
            d.join("alias/out"),
            root.join("prep/trace.json"),
            root.join("parts/0"),
        ];
        assert_eq!(digest_tree(&d.join("alias"), &other).unwrap(), digest);

        // without the exclusions the files are part of the tree
        let all = digest_tree(&root, &[]).unwrap();
        assert!(all.contains_key("out/report.json") && all.contains_key("prep/trace.json"));

        // one byte of one file, a new file and a link that points elsewhere
        write("src/lib.rs", "pub fn f() {} \n");
        let changed = digest_tree(&root, &exclude).unwrap();
        assert_ne!(changed["src/lib.rs"], digest["src/lib.rs"]);
        assert_eq!(changed["Cargo.lock"], digest["Cargo.lock"]);
        write("src/new.rs", "");
        assert!(
            digest_tree(&root, &exclude)
                .unwrap()
                .contains_key("src/new.rs")
        );
        std::fs::remove_file(root.join("link.rs")).unwrap();
        std::os::unix::fs::symlink("Cargo.lock", root.join("link.rs")).unwrap();
        assert_ne!(
            digest_tree(&root, &exclude).unwrap()["link.rs"],
            digest["link.rs"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

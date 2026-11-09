//! Build stage: turn a list of mutants into test binaries.
//!
//! Most mutants are embedded in one copy of the package (see [`crate::rewrite`])
//! that is built once. Mutants that make the embedded copy fail to compile are
//! removed again, guided by the compiler's error spans; the ones whose failure
//! may come from the embedding itself are built one at a time instead.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cargo_metadata::Artifact;
use serde::Serialize;

use crate::diag::{Diag, ErrSig, parse_messages};
use crate::mutant::Mutant;
use crate::rewrite::{FilePlan, Placement, RUNTIME_MODULE, Rendered, apply_single, placement};
use crate::sync::{Overrides, sync_tree, write_if_changed};
use crate::workspace::{Package, Workspace};

/// Environment variable that selects the active mutant in a schemata build.
pub const MUTANT_ENV: &str = "CARGO_SCHEMATA_MUTANT";

/// Errors with these codes can be caused by the embedding rather than by the
/// mutation itself, so such mutants get a build of their own. Syntax errors
/// (no code) are included.
const EMBEDDING_SUSPECT_CODES: &[&str] = &["E0308", "E0282", "E0283", "E0015", "E0658"];

pub struct BuildOptions {
    /// Extra arguments for cargo (what follows `--`), such as `--lib`.
    pub cargo_args: Vec<String>,
    /// Upper bound on `cargo check` / build runs spent on the embedded build.
    pub max_checks: usize,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            cargo_args: Vec::new(),
            max_checks: 40,
        }
    }
}

/// A test binary and how to run it the way `cargo test` would.
#[derive(Clone, Debug, Serialize)]
pub struct TestBinary {
    pub path: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub package: String,
    pub target: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    /// Runs of `cargo check` and of the embedded `cargo test --no-run`.
    pub embedded_runs: usize,
    /// Of those, runs spent probing to find which mutant an error belongs to.
    pub probe_runs: usize,
    /// The run budget ran out and the remaining mutants went to single builds.
    pub budget_exhausted: bool,
    pub copy_and_rewrite: Duration,
    pub embedded_build: Duration,
    pub single_builds: Duration,
    pub single_build_count: usize,
}

#[derive(Debug)]
pub struct Prepared {
    /// The directory the embedded build was made in.
    pub work_dir: PathBuf,
    /// Test binaries of the embedded build; run them with `CARGO_SCHEMATA_MUTANT=<id>`.
    pub test_binaries: Vec<TestBinary>,
    /// Ids of the mutants in the embedded build.
    pub embedded: Vec<usize>,
    /// Mutants built one at a time, with their own test binaries.
    pub single: Vec<(usize, Vec<TestBinary>)>,
    /// Mutants that do not compile, with the first error.
    pub unviable: Vec<(usize, String)>,
    /// Why each mutant was built on its own.
    pub single_why: BTreeMap<usize, String>,
    pub stats: Stats,
}

struct FileEntry {
    original: String,
    plan: Option<FilePlan>,
    is_root: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Check,
    Build,
}

struct Cargo {
    dir: PathBuf,
    target_dir: PathBuf,
    packages: Vec<String>,
    args: Vec<String>,
}

struct Run {
    success: bool,
    errors: Vec<Diag>,
    artifacts: Vec<Artifact>,
    stderr: String,
}

impl Cargo {
    fn run(&self, phase: Phase) -> Result<Run> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let mut cmd = Command::new(cargo);
        match phase {
            Phase::Check => cmd.args(["check", "--tests"]),
            Phase::Build => cmd.args(["test", "--no-run"]),
        };
        cmd.arg("--message-format=json");
        for p in &self.packages {
            cmd.arg(format!("--package={p}"));
        }
        cmd.args(&self.args)
            .current_dir(&self.dir)
            .env("CARGO_TARGET_DIR", &self.target_dir)
            .env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags())
            .env("INSTA_UPDATE", "no")
            .env_remove(MUTANT_ENV);
        let out = cmd.output().context("failed to run cargo")?;
        let parsed = parse_messages(&out.stdout);
        Ok(Run {
            success: out.status.success(),
            errors: parsed.errors,
            artifacts: parsed.artifacts,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// `--cap-lints=warn` added to whatever rustflags are set, so that the lints
/// the generated code trips (unreachable code, unused parentheses) and
/// `#![deny(warnings)]` do not stop the build.
fn encoded_rustflags() -> String {
    let cap = "--cap-lints=warn";
    if let Ok(encoded) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        if encoded.is_empty() {
            cap.to_owned()
        } else {
            format!("{encoded}\x1f{cap}")
        }
    } else if let Ok(flags) = std::env::var("RUSTFLAGS") {
        flags
            .split(' ')
            .filter(|s| !s.is_empty())
            .chain(std::iter::once(cap))
            .collect::<Vec<_>>()
            .join("\x1f")
    } else {
        cap.to_owned()
    }
}

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

struct Embedded<'a> {
    cargo: Cargo,
    files: BTreeMap<PathBuf, FileEntry>,
    work: &'a Path,
    arm_ids: Vec<usize>,
    runs: usize,
    probes: usize,
    max_runs: usize,
}

enum Mapped {
    Ids(BTreeSet<usize>),
    Candidates(Vec<usize>),
}

impl Embedded<'_> {
    fn render_all(&self, active: &BTreeSet<usize>) -> BTreeMap<PathBuf, Rendered> {
        self.files
            .iter()
            .map(|(rel, f)| {
                let mut r = match &f.plan {
                    Some(plan) => plan.render(active),
                    None => Rendered {
                        text: f.original.clone(),
                        ..Rendered::default()
                    },
                };
                if f.is_root {
                    r.text.push_str(RUNTIME_MODULE);
                }
                (rel.clone(), r)
            })
            .collect()
    }

    /// Write the files for `active` and run cargo on them.
    fn build(
        &mut self,
        active: &BTreeSet<usize>,
        phase: Phase,
    ) -> Result<(Run, BTreeMap<PathBuf, Rendered>)> {
        let rendered = self.render_all(active);
        for (rel, r) in &rendered {
            write_if_changed(&self.work.join(rel), &r.text)?;
        }
        self.runs += 1;
        Ok((self.cargo.run(phase)?, rendered))
    }

    fn map_error(
        &self,
        rendered: &BTreeMap<PathBuf, Rendered>,
        active: &BTreeSet<usize>,
        d: &Diag,
    ) -> Mapped {
        let mut ids = BTreeSet::new();
        let mut group_ids: BTreeSet<usize> = BTreeSet::new();
        for (file, range) in &d.spans {
            let p = Path::new(file);
            let p = p.strip_prefix(self.work).unwrap_or(p);
            let p = p.strip_prefix("./").unwrap_or(p);
            let Some(r) = rendered.get(p) else { continue };
            let inside = |outer: &std::ops::Range<usize>| {
                outer.start <= range.start && range.end <= outer.end
            };
            if let Some(b) = r
                .branches
                .iter()
                .filter(|b| inside(&b.range))
                .min_by_key(|b| b.range.len())
            {
                ids.insert(b.id);
            } else if let Some(g) = r
                .groups
                .iter()
                .filter(|g| inside(&g.range))
                .min_by_key(|g| g.range.len())
            {
                group_ids.extend(g.ids.iter().copied());
            }
        }
        if !ids.is_empty() {
            return Mapped::Ids(ids);
        }
        if !group_ids.is_empty() {
            return Mapped::Candidates(group_ids.into_iter().collect());
        }
        if d.code.as_deref() == Some("E0004") {
            let arms: Vec<usize> = self
                .arm_ids
                .iter()
                .copied()
                .filter(|id| active.contains(id))
                .collect();
            if !arms.is_empty() {
                return Mapped::Candidates(arms);
            }
        }
        Mapped::Candidates(active.iter().copied().collect())
    }

    /// Find the one mutant among `cands` that causes `sig`, by removing halves.
    /// Returns `None` when the run budget ends first.
    fn bisect(
        &mut self,
        active: &BTreeSet<usize>,
        mut cands: Vec<usize>,
        sig: &ErrSig,
        phase: Phase,
    ) -> Result<Option<usize>> {
        if cands.len() > 1 && cands.len() == active.len() {
            // Nothing narrows this down: make sure the error is ours at all.
            if self.runs >= self.max_runs {
                return Ok(None);
            }
            self.probes += 1;
            let (run, _) = self.build(&BTreeSet::new(), phase)?;
            if run.errors.iter().any(|e| &e.sig() == sig) {
                bail!("the package does not build even without mutants: {}", sig.1);
            }
        }
        while cands.len() > 1 {
            if self.runs >= self.max_runs {
                return Ok(None);
            }
            let b = cands.split_off(cands.len() / 2);
            let without: BTreeSet<usize> = active
                .iter()
                .copied()
                .filter(|id| !cands.contains(id))
                .collect();
            self.probes += 1;
            let (run, _) = self.build(&without, phase)?;
            if run.errors.iter().any(|e| &e.sig() == sig) {
                // Still failing without the first half: the culprit is in `b`.
                cands = b;
            }
            // else: the error went away with `cands` removed, so it is in `cands`
        }
        Ok(cands.first().copied())
    }
}

/// Why a mutant left the embedded build.
enum Cause {
    Error(ErrSig),
    Budget,
}

pub fn prepare(
    workspace: &Workspace,
    packages: &[Arc<Package>],
    mutants: &[Mutant],
    opts: &BuildOptions,
) -> Result<Prepared> {
    let t_start = Instant::now();
    let root = workspace.root().as_std_path().to_owned();
    let target_dir = workspace.target_dir().as_std_path().to_owned();
    let schemata_dir = target_dir.join("schemata");
    let work = schemata_dir.join("work");
    let by_id: HashMap<usize, &Mutant> = mutants.iter().map(|m| (m.id, m)).collect();

    let mut single_why: BTreeMap<usize, String> = BTreeMap::new();
    let mut embed_by_file: BTreeMap<PathBuf, Vec<&Mutant>> = BTreeMap::new();
    for m in mutants {
        match placement(m) {
            Placement::Embed => embed_by_file
                .entry(m.file.as_std_path().to_owned())
                .or_default()
                .push(m),
            Placement::Single => {
                single_why.insert(m.id, format!("{:?} / {:?}", m.genre, m.context));
            }
        }
    }

    let mut files: BTreeMap<PathBuf, FileEntry> = BTreeMap::new();
    let mut arm_ids = Vec::new();
    let mut active: BTreeSet<usize> = BTreeSet::new();
    for (rel, ms) in &embed_by_file {
        let original = std::fs::read_to_string(root.join(rel))
            .with_context(|| format!("read {}", rel.display()))?;
        let (plan, rejected) = FilePlan::new(&original, ms);
        for (id, why) in rejected {
            single_why.insert(id, why);
        }
        arm_ids.extend(plan.arm_ids());
        let rejected_ids: BTreeSet<usize> = single_why.keys().copied().collect();
        active.extend(
            ms.iter()
                .map(|m| m.id)
                .filter(|id| !rejected_ids.contains(id)),
        );
        files.insert(
            rel.clone(),
            FileEntry {
                original,
                plan: Some(plan),
                is_root: false,
            },
        );
    }
    let used_packages: BTreeSet<&str> = mutants.iter().map(|m| m.package.as_str()).collect();
    for p in packages
        .iter()
        .filter(|p| used_packages.contains(p.name.as_str()))
    {
        for src in &p.all_sources {
            let rel = src.as_std_path().to_owned();
            if !root.join(&rel).is_file() {
                continue;
            }
            if let Some(f) = files.get_mut(&rel) {
                f.is_root = true;
            } else {
                files.insert(
                    rel.clone(),
                    FileEntry {
                        original: std::fs::read_to_string(root.join(&rel))
                            .with_context(|| format!("read {}", rel.display()))?,
                        plan: None,
                        is_root: true,
                    },
                );
            }
        }
    }
    let package_names: Vec<String> = used_packages.iter().map(|s| (*s).to_owned()).collect();

    let mut emb = Embedded {
        cargo: Cargo {
            dir: work.clone(),
            target_dir: schemata_dir.join("target"),
            packages: package_names.clone(),
            args: opts.cargo_args.clone(),
        },
        files,
        work: &work,
        arm_ids,
        runs: 0,
        probes: 0,
        max_runs: opts.max_checks,
    };

    // 1. copy the tree, with the rewritten files already in place
    let initial: Overrides = emb
        .render_all(&active)
        .into_iter()
        .map(|(rel, r)| (rel, r.text))
        .collect();
    sync_tree(&root, &work, std::slice::from_ref(&target_dir), &initial)?;
    let copy_and_rewrite = t_start.elapsed();

    // 2. check, drop what does not compile, build
    let t_embedded = Instant::now();
    let mut removed: BTreeMap<usize, Cause> = BTreeMap::new();
    let mut phase = Phase::Check;
    let mut budget_exhausted = false;
    let artifacts = loop {
        let (run, rendered) = emb.build(&active, phase)?;
        if run.errors.is_empty() {
            if !run.success {
                bail!("cargo failed without a diagnostic:\n{}", tail(&run.stderr));
            }
            if phase == Phase::Check {
                phase = Phase::Build;
                continue;
            }
            break run.artifacts;
        }
        if active.is_empty() {
            bail!(
                "the package does not build even without mutants: {}",
                run.errors[0].describe()
            );
        }
        let mut kill: BTreeMap<usize, ErrSig> = BTreeMap::new();
        let mut pending: Option<(ErrSig, Vec<usize>)> = None;
        for d in &run.errors {
            match emb.map_error(&rendered, &active, d) {
                Mapped::Ids(ids) => {
                    for id in ids {
                        kill.entry(id).or_insert_with(|| d.sig());
                    }
                }
                Mapped::Candidates(c) => {
                    if pending.is_none() {
                        pending = Some((d.sig(), c));
                    }
                }
            }
        }
        if kill.is_empty() {
            let (sig, cands) = pending.expect("an unmapped error");
            if cands.len() == 1 {
                kill.insert(cands[0], sig);
            } else {
                match emb.bisect(&active, cands, &sig, phase)? {
                    Some(id) => {
                        kill.insert(id, sig);
                    }
                    None => budget_exhausted = true,
                }
            }
        }
        if !budget_exhausted && emb.runs >= emb.max_runs {
            budget_exhausted = true;
        }
        if budget_exhausted {
            for id in std::mem::take(&mut active) {
                removed.insert(id, Cause::Budget);
            }
        } else {
            for (id, sig) in kill {
                active.remove(&id);
                removed.insert(id, Cause::Error(sig));
            }
        }
        phase = Phase::Check;
    };
    let embedded_build = t_embedded.elapsed();

    let meta = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .current_dir(&work)
        .exec()
        .context("cargo metadata on the work directory")?;
    let test_binaries = binaries(&artifacts, &meta, &emb.cargo.target_dir, None)?;

    let mut unviable: Vec<(usize, String)> = Vec::new();
    for (id, cause) in removed {
        match cause {
            Cause::Budget => {
                single_why.insert(id, "run budget exhausted".into());
            }
            Cause::Error((code, message)) => {
                let suspect = code
                    .as_deref()
                    .is_none_or(|c| EMBEDDING_SUSPECT_CODES.contains(&c));
                let text = match &code {
                    Some(c) => format!("{c}: {message}"),
                    None => message,
                };
                if suspect {
                    single_why.insert(id, format!("embedded build failed ({text})"));
                } else {
                    unviable.push((id, text));
                }
            }
        }
    }

    // 3. one at a time
    let t_single = Instant::now();
    let single_work = schemata_dir.join("single-work");
    let single_target = schemata_dir.join("single-target");
    let single_out = schemata_dir.join("single");
    let single_cargo = Cargo {
        dir: single_work.clone(),
        target_dir: single_target.clone(),
        packages: package_names,
        args: opts.cargo_args.clone(),
    };
    let mut single: Vec<(usize, Vec<TestBinary>)> = Vec::new();
    let single_ids: Vec<usize> = single_why.keys().copied().collect();
    let mut single_meta: Option<cargo_metadata::Metadata> = None;
    if single_out.exists() {
        std::fs::remove_dir_all(&single_out)?;
    }
    for &id in &single_ids {
        let m = by_id[&id];
        let rel = m.file.as_std_path().to_owned();
        let original = match emb.files.get(&rel) {
            Some(f) => f.original.clone(),
            None => std::fs::read_to_string(root.join(&rel))
                .with_context(|| format!("read {}", rel.display()))?,
        };
        let mut ov = Overrides::new();
        ov.insert(rel, apply_single(&original, m));
        sync_tree(&root, &single_work, std::slice::from_ref(&target_dir), &ov)?;
        let run = single_cargo.run(Phase::Build)?;
        if !run.success || !run.errors.is_empty() {
            let why = run
                .errors
                .first()
                .map(Diag::describe)
                .unwrap_or_else(|| tail(&run.stderr));
            unviable.push((id, why));
            continue;
        }
        if single_meta.is_none() {
            single_meta = Some(
                cargo_metadata::MetadataCommand::new()
                    .no_deps()
                    .current_dir(&single_work)
                    .exec()
                    .context("cargo metadata on the single-build directory")?,
            );
        }
        let out_dir = single_out.join(id.to_string());
        std::fs::create_dir_all(&out_dir)?;
        let bins = binaries(
            &run.artifacts,
            single_meta.as_ref().expect("metadata"),
            &single_target,
            Some(&out_dir),
        )?;
        single.push((id, bins));
    }
    let single_builds = t_single.elapsed();
    let single_count = single_ids.len();
    // Mutants that turned out unviable no longer count as single.
    let unviable_ids: BTreeSet<usize> = unviable.iter().map(|(id, _)| *id).collect();
    single_why.retain(|id, _| !unviable_ids.contains(id));
    unviable.sort();

    Ok(Prepared {
        work_dir: work.clone(),
        test_binaries,
        embedded: active.into_iter().collect(),
        single,
        unviable,
        single_why,
        stats: Stats {
            embedded_runs: emb.runs,
            probe_runs: emb.probes,
            budget_exhausted,
            copy_and_rewrite,
            embedded_build,
            single_builds,
            single_build_count: single_count,
        },
    })
}

/// The test binaries among `artifacts`, with the environment `cargo test`
/// would give them. With `copy_to`, the executables are copied there first
/// (the build directory is reused by the next single build).
fn binaries(
    artifacts: &[Artifact],
    meta: &cargo_metadata::Metadata,
    target_dir: &Path,
    copy_to: Option<&Path>,
) -> Result<Vec<TestBinary>> {
    let place = |exe: &Path| -> Result<PathBuf> {
        match copy_to {
            Some(dir) => {
                let dst = dir.join(exe.file_name().expect("executable name"));
                std::fs::copy(exe, &dst).with_context(|| format!("copy {}", exe.display()))?;
                Ok(dst)
            }
            None => Ok(exe.to_owned()),
        }
    };
    // Binaries of the packages, for CARGO_BIN_EXE_<name>.
    let mut bin_exes: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    for a in artifacts {
        let Some(exe) = &a.executable else { continue };
        if a.profile.test || !a.target.is_bin() {
            continue;
        }
        bin_exes
            .entry(a.package_id.repr.clone())
            .or_default()
            .push((a.target.name.clone(), place(exe.as_std_path())?));
    }
    let mut out: Vec<TestBinary> = Vec::new();
    for a in artifacts {
        let Some(exe) = &a.executable else { continue };
        if !a.profile.test {
            continue;
        }
        let Some(pkg) = meta.packages.iter().find(|p| p.id == a.package_id) else {
            continue;
        };
        let manifest_dir = pkg
            .manifest_path
            .parent()
            .expect("manifest has a parent")
            .as_std_path()
            .to_owned();
        let path = place(exe.as_std_path())?;
        let mut env: Vec<(String, String)> = vec![
            (
                "CARGO_MANIFEST_DIR".into(),
                manifest_dir.to_string_lossy().into_owned(),
            ),
            ("CARGO_PKG_NAME".into(), pkg.name.to_string()),
            ("CARGO_PKG_VERSION".into(), pkg.version.to_string()),
            (
                "CARGO_PKG_VERSION_MAJOR".into(),
                pkg.version.major.to_string(),
            ),
            (
                "CARGO_PKG_VERSION_MINOR".into(),
                pkg.version.minor.to_string(),
            ),
            (
                "CARGO_PKG_VERSION_PATCH".into(),
                pkg.version.patch.to_string(),
            ),
            ("CARGO_PKG_VERSION_PRE".into(), pkg.version.pre.to_string()),
            ("CARGO_PKG_AUTHORS".into(), pkg.authors.join(":")),
            (
                "CARGO_PKG_DESCRIPTION".into(),
                pkg.description.clone().unwrap_or_default(),
            ),
            (
                "CARGO_PKG_HOMEPAGE".into(),
                pkg.homepage.clone().unwrap_or_default(),
            ),
            (
                "CARGO_PKG_REPOSITORY".into(),
                pkg.repository.clone().unwrap_or_default(),
            ),
            (
                "CARGO_PKG_LICENSE".into(),
                pkg.license.clone().unwrap_or_default(),
            ),
            (
                "CARGO_PKG_LICENSE_FILE".into(),
                pkg.license_file
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
            (
                "CARGO_PKG_RUST_VERSION".into(),
                pkg.rust_version
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
            ("CARGO_CRATE_NAME".into(), a.target.name.replace('-', "_")),
            ("CARGO_PRIMARY_PACKAGE".into(), "1".into()),
        ];
        if a.target.kind.iter().any(|k| k.to_string() == "test") {
            let tmp = target_dir.join("tmp");
            std::fs::create_dir_all(&tmp)?;
            env.push((
                "CARGO_TARGET_TMPDIR".into(),
                tmp.to_string_lossy().into_owned(),
            ));
        }
        for (name, p) in bin_exes.get(&a.package_id.repr).into_iter().flatten() {
            env.push((
                format!("CARGO_BIN_EXE_{name}"),
                p.to_string_lossy().into_owned(),
            ));
        }
        if let Some(deps) = exe.as_std_path().parent() {
            let mut dirs = vec![deps.to_owned()];
            if let Some(profile) = deps.parent() {
                dirs.push(profile.to_owned());
            }
            let var = if cfg!(target_os = "macos") {
                "DYLD_FALLBACK_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            if let Ok(joined) = std::env::join_paths(dirs.into_iter().chain(std::env::split_paths(
                &std::env::var_os(var).unwrap_or_default(),
            ))) {
                env.push((var.into(), joined.to_string_lossy().into_owned()));
            }
        }
        if out.iter().any(|b| b.path == path.to_string_lossy()) {
            continue;
        }
        out.push(TestBinary {
            path: path.to_string_lossy().into_owned(),
            cwd: manifest_dir.to_string_lossy().into_owned(),
            env,
            package: pkg.name.to_string(),
            target: a.target.name.clone(),
        });
    }
    Ok(out)
}

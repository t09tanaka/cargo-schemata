//! What a run found, and how it is written: the report in the
//! mutation-testing-elements schema (version 2), the `*.txt` lists of
//! cargo-mutants, the lines on the terminal.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::mutant::Mutant;
use crate::shard::Sharding;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Caught,
    Missed,
    Timeout,
    Unviable,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Caught => "caught",
            Status::Missed => "missed",
            Status::Timeout => "timeout",
            Status::Unviable => "unviable",
        }
    }

    /// The status in the schema.
    fn element(self) -> &'static str {
        match self {
            Status::Caught => "Killed",
            Status::Missed => "Survived",
            Status::Timeout => "Timeout",
            Status::Unviable => "CompileError",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MutantResult {
    pub id: usize,
    pub status: Status,
    pub duration: Duration,
    /// Why, for an unviable mutant (the compiler's first error), a missed one
    /// that no test reached, or a time-out ([`timeout_reason`]).
    pub reason: Option<String>,
}

/// The reason of a time-out: the limit this mutant had.
pub fn timeout_reason(limit: Duration) -> String {
    format!("did not end within {}s", limit.as_secs())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub caught: usize,
    pub missed: usize,
    pub timeout: usize,
    pub unviable: usize,
}

impl Counts {
    pub fn of(results: &[MutantResult]) -> Counts {
        let mut c = Counts::default();
        for r in results {
            match r.status {
                Status::Caught => c.caught += 1,
                Status::Missed => c.missed += 1,
                Status::Timeout => c.timeout += 1,
                Status::Unviable => c.unviable += 1,
            }
        }
        c
    }

    pub fn text(&self, elapsed: Duration) -> String {
        format!(
            "{} caught, {} missed, {} timeout, {} unviable in {:.1}s",
            self.caught,
            self.missed,
            self.timeout,
            self.unviable,
            elapsed.as_secs_f64()
        )
    }
}

/// The files of a run's output directory.
pub const OUTPUT_FILES: [&str; 5] = [
    "report.json",
    "caught.txt",
    "missed.txt",
    "timeout.txt",
    "unviable.txt",
];

/// Removes what an earlier run wrote. Only the known files: the directory may be
/// one the user keeps other things in.
pub fn clear_output(dir: &Path) -> Result<()> {
    for f in OUTPUT_FILES {
        match std::fs::remove_file(dir.join(f)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", dir.join(f).display())),
        }
    }
    Ok(())
}

/// The report, in the mutation-testing-elements schema, version 2.
///
/// For one part of a divided run (`shard` is `(k, n, how it was divided)`), `mutants` is the whole
/// list: the ones of the other parts have no result and are `Pending`, and
/// `config.shard` says which part this is and the order of the whole list, which
/// `merge` needs. Without `shard`, a mutant without a result is left out.
pub fn report_json(
    mutants: &[Mutant],
    results: &[MutantResult],
    shard: Option<(usize, usize, Sharding)>,
) -> Value {
    let mut order: Vec<String> = Vec::new();
    let by_id: BTreeMap<usize, &MutantResult> = results.iter().map(|r| (r.id, r)).collect();
    let mut files: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for m in mutants {
        let r = by_id.get(&m.id);
        if r.is_none() && shard.is_none() {
            continue;
        }
        let path = m.file_slashes();
        let entry = files
            .entry(path.clone())
            .or_insert_with(|| (m.code.to_string(), Vec::new()));
        let mut v = json!({
            "id": format!("{path}#{}", entry.1.len()),
            "mutatorName": format!("{:?}", m.genre),
            "replacement": m.replacement,
            "description": m.describe_change(),
            "location": {
                "start": {"line": m.span.start.line, "column": m.span.start.column},
                "end": {"line": m.span.end.line, "column": m.span.end.column},
            },
            "status": r.map_or("Pending", |r| r.status.element()),
            "duration": r.map_or(0, |r| r.duration.as_millis() as u64),
        });
        let reason = r.and_then(|r| r.reason.clone());
        order.push(v["id"].as_str().unwrap_or_default().to_owned());
        if let Some(reason) = reason {
            v["statusReason"] = Value::String(reason);
        }
        entry.1.push(v);
    }
    let files: serde_json::Map<String, Value> = files
        .into_iter()
        .map(|(path, (source, mutants))| {
            (
                path,
                json!({"language": "rust", "source": source, "mutants": mutants}),
            )
        })
        .collect();
    let mut report = json!({
        "schemaVersion": "2",
        "thresholds": {"high": 100, "low": 100},
        "framework": {"name": "cargo-schemata", "version": env!("CARGO_PKG_VERSION")},
        "files": files,
    });
    if let Some((k, n, sharding)) = shard {
        report["config"] = json!({"shard": {
            "index": k, "count": n, "sharding": sharding.name(), "order": order,
        }});
    }
    report
}

/// One list of names per status, like the files cargo-mutants writes.
pub fn name_list(mutants: &[Mutant], results: &[MutantResult], status: Status) -> String {
    let by_id: BTreeMap<usize, &Mutant> = mutants.iter().map(|m| (m.id, m)).collect();
    let mut ids: Vec<usize> = results
        .iter()
        .filter(|r| r.status == status)
        .map(|r| r.id)
        .collect();
    ids.sort_unstable();
    ids.into_iter()
        .filter_map(|id| by_id.get(&id))
        .map(|m| m.name(true) + "\n")
        .collect()
}

pub fn write_output(
    dir: &Path,
    mutants: &[Mutant],
    results: &[MutantResult],
    report: &Value,
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let write = |name: &str, text: String| {
        std::fs::write(dir.join(name), text).with_context(|| format!("write {name}"))
    };
    write(
        "report.json",
        serde_json::to_string_pretty(report).expect("serialize report") + "\n",
    )?;
    for (name, status) in [
        ("caught.txt", Status::Caught),
        ("missed.txt", Status::Missed),
        ("timeout.txt", Status::Timeout),
        ("unviable.txt", Status::Unviable),
    ] {
        write(name, name_list(mutants, results, status))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: usize, status: Status) -> MutantResult {
        MutantResult {
            id,
            status,
            duration: Duration::from_millis(5),
            reason: None,
        }
    }

    #[test]
    fn counts_each_status() {
        let c = Counts::of(&[
            r(0, Status::Caught),
            r(1, Status::Caught),
            r(2, Status::Missed),
            r(3, Status::Timeout),
            r(4, Status::Unviable),
        ]);
        assert_eq!(
            c,
            Counts {
                caught: 2,
                missed: 1,
                timeout: 1,
                unviable: 1
            }
        );
        assert_eq!(
            c.text(Duration::from_millis(1250)),
            "2 caught, 1 missed, 1 timeout, 1 unviable in 1.2s"
        );
    }

    #[test]
    fn statuses_map_to_the_schema() {
        let names: Vec<&str> = [
            Status::Caught,
            Status::Missed,
            Status::Timeout,
            Status::Unviable,
        ]
        .into_iter()
        .map(Status::element)
        .collect();
        assert_eq!(names, ["Killed", "Survived", "Timeout", "CompileError"]);
        assert_eq!(Status::Missed.word(), "missed");
        assert_eq!(Status::Caught.word(), "caught");
        assert_eq!(Status::Timeout.word(), "timeout");
        assert_eq!(Status::Unviable.word(), "unviable");
    }

    #[test]
    fn clear_output_removes_only_known_files() {
        let dir = std::env::temp_dir().join(format!("schemata-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in OUTPUT_FILES {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        std::fs::write(dir.join("mine.txt"), "keep").unwrap();
        clear_output(&dir).unwrap();
        clear_output(&dir).unwrap();
        assert!(OUTPUT_FILES.iter().all(|f| !dir.join(f).exists()));
        assert!(dir.join("mine.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

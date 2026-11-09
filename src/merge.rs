//! `cargo schemata merge`: the reports of the parts of a run divided with
//! `--shard` become the report of the whole run.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use serde_json::Value;

use crate::results::{self, OUTPUT_FILES};
use crate::shard::Shard;

/// One part: where it was read from, which part it is, and the report.
struct Part {
    path: String,
    index: usize,
    count: usize,
    order: Vec<String>,
    report: Value,
}

/// Fields of a mutant that depend on the part that evaluated it.
const RESULT_KEYS: [&str; 3] = ["status", "duration", "statusReason"];

fn read_part(path: &Utf8PathBuf) -> Result<Part, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: cannot be read: {e}"))?;
    let report: Value =
        serde_json::from_str(&text).map_err(|e| format!("{path}: not a report: {e}"))?;
    let shard = &report["config"]["shard"];
    let (Some(index), Some(count), Some(order)) = (
        shard["index"].as_u64(),
        shard["count"].as_u64(),
        shard["order"].as_array(),
    ) else {
        return Err(format!(
            "{path}: not the report of a part (a run with `--shard` writes `config.shard`)"
        ));
    };
    let order: Option<Vec<String>> = order
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect();
    let (index, count) = (index as usize, count as usize);
    match order {
        Some(order) if count > 0 && index < count && report["files"].is_object() => Ok(Part {
            path: path.to_string(),
            index,
            count,
            order,
            report,
        }),
        _ => Err(format!(
            "{path}: not the report of a part (`config.shard` is wrong)"
        )),
    }
}

/// A report without what only the part that evaluated a mutant knows.
fn skeleton(report: &Value) -> Value {
    let mut s = report.clone();
    if let Some(o) = s.as_object_mut() {
        o.remove("config");
    }
    if let Some(files) = s["files"].as_object_mut() {
        for file in files.values_mut() {
            if let Some(mutants) = file["mutants"].as_array_mut() {
                for m in mutants {
                    if let Some(o) = m.as_object_mut() {
                        for k in RESULT_KEYS {
                            o.remove(k);
                        }
                    }
                }
            }
        }
    }
    s
}

/// Where a mutant id is in the report: its file and its number in the file.
fn locate(report: &Value) -> HashMap<String, (String, usize)> {
    let mut at = HashMap::new();
    for (path, file) in report["files"].as_object().into_iter().flatten() {
        for (i, m) in file["mutants"].as_array().into_iter().flatten().enumerate() {
            if let Some(id) = m["id"].as_str() {
                at.insert(id.to_owned(), (path.clone(), i));
            }
        }
    }
    at
}

fn status_of(m: &Value) -> &str {
    m["status"].as_str().unwrap_or_default()
}

/// Look at the parts: what is wrong with them as the parts of one run, at most
/// twenty lines. Empty if they are all the parts of one run.
fn problems(parts: &[Part]) -> Vec<String> {
    let first = &parts[0];
    let count = first.count;
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        if p.count != count {
            out.push(format!(
                "{} is part {}/{} but {} is part {}/{}: the runs were divided differently",
                p.path, p.index, p.count, first.path, first.index, first.count
            ));
        }
    }
    if !out.is_empty() {
        return out;
    }
    for k in 0..count {
        let given: Vec<&str> = parts
            .iter()
            .filter(|p| p.index == k)
            .map(|p| p.path.as_str())
            .collect();
        match given.len() {
            0 => out.push(format!("shard {k}/{count} is missing")),
            1 => {}
            _ => out.push(format!(
                "shard {k}/{count} is given more than once ({})",
                given.join(", ")
            )),
        }
    }
    if !out.is_empty() {
        return out;
    }
    let want = skeleton(&first.report);
    for p in &parts[1..] {
        if p.order != first.order || skeleton(&p.report) != want {
            out.push(format!(
                "{} is not from the same run as {}: the mutants differ",
                p.path, first.path
            ));
        }
    }
    if !out.is_empty() {
        return out;
    }
    // each mutant is evaluated by the part it belongs to, and by no other
    let at = locate(&first.report);
    if at.len() != first.order.len() || first.order.iter().any(|id| !at.contains_key(id)) {
        out.push(format!(
            "{}: the order of the mutants is not the mutants of the report",
            first.path
        ));
        return out;
    }
    for p in parts {
        let own = Shard {
            k: p.index,
            n: count,
        }
        .range(first.order.len());
        for (pos, id) in first.order.iter().enumerate() {
            let (path, i) = &at[id];
            let m = &p.report["files"][path]["mutants"][*i];
            let pending = status_of(m) == "Pending";
            if own.contains(&pos) == pending {
                out.push(format!(
                    "{}: {id} is {} in part {}/{count}",
                    p.path,
                    if pending {
                        "pending but belongs to it"
                    } else {
                        "evaluated but belongs to another part"
                    },
                    p.index
                ));
            }
        }
    }
    out
}

fn print_problems(lines: &[String]) {
    eprintln!("cargo schemata merge: the parts are not the parts of one run");
    for l in lines.iter().take(20) {
        eprintln!("  - {l}");
    }
    if lines.len() > 20 {
        eprintln!("  - ... and {} more", lines.len() - 20);
    }
    eprintln!("cargo schemata merge: FAILED (nothing was merged)");
}

/// Puts the parts together and writes the output. The exit code: 0, 1 if a mutant
/// was missed or the parts are not the parts of one run (nothing is written then).
pub fn merge(paths: &[Utf8PathBuf], out_dir: &Path, json: bool) -> Result<i32> {
    // Nothing old may be read as the result.
    results::clear_output(out_dir)?;
    let mut parts = Vec::new();
    let mut bad = Vec::new();
    for path in paths {
        match read_part(path) {
            Ok(p) => parts.push(p),
            Err(e) => bad.push(e),
        }
    }
    if bad.is_empty() {
        bad = problems(&parts);
    }
    if !bad.is_empty() {
        print_problems(&bad);
        return Ok(1);
    }

    let first = &parts[0];
    let total = first.order.len();
    let at = locate(&first.report);
    let by_index: BTreeMap<usize, &Part> = parts.iter().map(|p| (p.index, p)).collect();
    let mut merged = first.report.clone();
    merged.as_object_mut().expect("a report").remove("config");
    let mut lists: BTreeMap<&str, String> = BTreeMap::new();
    let mut counts = [0usize; 4];
    for (pos, id) in first.order.iter().enumerate() {
        let owner = (0..parts.len())
            .find(|&k| Shard { k, n: first.count }.range(total).contains(&pos))
            .expect("every position belongs to a part");
        let (path, i) = &at[id];
        let m = by_index[&owner].report["files"][path]["mutants"][*i].clone();
        let (file, word) = match status_of(&m) {
            "Killed" => ("caught.txt", 0),
            "Survived" => ("missed.txt", 1),
            "Timeout" => ("timeout.txt", 2),
            _ => ("unviable.txt", 3),
        };
        counts[word] += 1;
        let loc = &m["location"]["start"];
        lists.entry(file).or_default().push_str(&format!(
            "{path}:{}:{}: {}\n",
            loc["line"],
            loc["column"],
            m["description"].as_str().unwrap_or_default()
        ));
        merged["files"][path]["mutants"][*i] = m;
    }

    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let write = |name: &str, text: String| {
        std::fs::write(out_dir.join(name), text).with_context(|| format!("write {name}"))
    };
    write(
        "report.json",
        serde_json::to_string_pretty(&merged).expect("serialize report") + "\n",
    )?;
    for name in &OUTPUT_FILES[1..] {
        write(name, lists.remove(name).unwrap_or_default())?;
    }

    let [caught, missed, timeout, unviable] = counts;
    let summary = format!(
        "{} part(s) merged: {caught} caught, {missed} missed, {timeout} timeout, {unviable} unviable",
        parts.len()
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&merged).expect("serialize report")
        );
    }
    if missed > 0 {
        eprintln!("cargo schemata merge: {missed} surviving mutant(s)");
        for line in lines_of(out_dir, "missed.txt") {
            eprintln!("{line} (missed)");
        }
        eprintln!("cargo schemata merge: FAILED ({summary})");
        Ok(1)
    } else {
        let line = format!("cargo schemata merge: OK ({summary})");
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
        Ok(0)
    }
}

fn lines_of(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

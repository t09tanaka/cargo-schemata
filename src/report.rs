//! The `--build-only` report: how each mutant was built, and what it cost.

use std::fmt::Write;
use std::time::Duration;

use serde_json::json;

use crate::mutant::Mutant;
use crate::prepare::Prepared;

fn secs(d: Duration) -> String {
    format!("{:.2}s", d.as_secs_f64())
}

fn ids(v: impl Iterator<Item = usize>) -> String {
    v.map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

pub fn report(mutants: &[Mutant], p: &Prepared, json: bool) -> String {
    if json {
        let name = |id: usize| mutants.iter().find(|m| m.id == id).map(|m| m.name(true));
        let value = json!({
            "work_dir": p.work_dir,
            "test_binaries": p.test_binaries,
            "embedded": p.embedded.iter().map(|&id| json!({"id": id, "name": name(id)})).collect::<Vec<_>>(),
            "single": p.single.iter().map(|(id, bins)| json!({
                "id": id, "name": name(*id), "why": p.single_why.get(id), "test_binaries": bins,
            })).collect::<Vec<_>>(),
            "unviable": p.unviable.iter().map(|(id, why)| json!({"id": id, "name": name(*id), "reason": why})).collect::<Vec<_>>(),
            "stats": {
                "embedded_runs": p.stats.embedded_runs,
                "probe_runs": p.stats.probe_runs,
                "budget_exhausted": p.stats.budget_exhausted,
                "copy_and_rewrite_secs": p.stats.copy_and_rewrite.as_secs_f64(),
                "embedded_build_secs": p.stats.embedded_build.as_secs_f64(),
                "single_builds_secs": p.stats.single_builds.as_secs_f64(),
                "single_build_count": p.stats.single_build_count,
            },
        });
        return serde_json::to_string_pretty(&value).expect("serialize report") + "\n";
    }
    let mut s = String::new();
    let _ = writeln!(s, "mutants: {}", mutants.len());
    let _ = writeln!(s, "embedded: {}", p.embedded.len());
    let _ = writeln!(s, "  ids: {}", ids(p.embedded.iter().copied()));
    let _ = writeln!(s, "single: {}", p.single_why.len());
    for (id, why) in &p.single_why {
        let _ = writeln!(s, "  {id}: {why}");
    }
    let _ = writeln!(s, "unviable: {}", p.unviable.len());
    for (id, why) in &p.unviable {
        let first = why.lines().next().unwrap_or("");
        let _ = writeln!(s, "  {id}: {first}");
    }
    let st = &p.stats;
    let _ = writeln!(
        s,
        "embedded runs: {} (probing: {}){}",
        st.embedded_runs,
        st.probe_runs,
        if st.budget_exhausted {
            ", budget exhausted"
        } else {
            ""
        }
    );
    let _ = writeln!(s, "test binaries: {}", p.test_binaries.len());
    let _ = writeln!(s, "time copy and rewrite: {}", secs(st.copy_and_rewrite));
    let _ = writeln!(
        s,
        "time embedded check and build: {}",
        secs(st.embedded_build)
    );
    let _ = writeln!(
        s,
        "time single builds: {} ({} built)",
        secs(st.single_builds),
        st.single_build_count
    );
    s
}

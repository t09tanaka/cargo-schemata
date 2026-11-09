// Copyright 2023-2024 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! List mutants as text or json.

use crate::mutant::Mutant;

/// Return a string representation of a list of mutants: one name per line, or
/// the pretty-printed JSON array of `cargo mutants --list --json`.
pub fn list_mutants(mutants: &[Mutant], json: bool) -> String {
    if json {
        let list: Vec<serde_json::Value> = mutants.iter().map(Mutant::to_json).collect();
        serde_json::to_string_pretty(&list).expect("Serialize mutants")
    } else {
        mutants.iter().map(|m| m.name(true) + "\n").collect()
    }
}

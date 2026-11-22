/// Two `match`es over the same `Option<u8>`, each with a `_` that has a guard,
/// so rustc words the E0004 of one exactly like that of the other once their
/// arms are deleted.
///
/// Deleting `None` or `Some(x)` here leaves the match non-exhaustive;
/// `Some(0)` comes last and changes nothing.
pub fn earlier(o: Option<u8>, skip: bool) -> u8 {
    match o {
        _ if skip => 1,
        None => 9,
        Some(x) => x,
        #[allow(unreachable_patterns)]
        Some(0) => 0,
    }
}

/// The same, with the harmless arm first.
pub fn later(o: Option<u8>, skip: bool) -> u8 {
    match o {
        Some(0) => 0,
        _ if skip => 1,
        Some(x) => x,
        None => 9,
    }
}

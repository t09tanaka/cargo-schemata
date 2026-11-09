//! A small crate with one mutant of every outcome.

/// Tested thoroughly: its mutants are caught.
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

/// Tested with one input only: `x + 2` gives the same answer, so that mutant is missed.
pub fn twice(x: u32) -> u32 {
    x * 2
}

/// `+=` becomes `*=`: the counter stays at zero and the loop never ends.
pub fn count_to(n: u32) -> u32 {
    let mut i = 0;
    while i < n {
        i += 1;
    }
    i
}

/// `+` becomes `*`: that is not a program.
pub fn join(a: &str, b: &str) -> String {
    a.to_owned() + b
}

/// A const fn is built on its own. The tests never look at it, so its mutants are missed.
pub const fn unchecked() -> u32 {
    5
}

/// A const fn whose value is checked, by the library test and through the binary.
pub const fn limit() -> u32 {
    10
}

/// Starts a process that outlives the test, and records its pid.
pub fn leave_a_child_behind() {
    let child = std::process::Command::new("sleep")
        .arg("86400.123")
        .spawn()
        .unwrap();
    if let Some(path) = std::env::var_os("RUN_KIT_PIDS") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(format!("{}\n", child.id()).as_bytes()).unwrap();
    }
}

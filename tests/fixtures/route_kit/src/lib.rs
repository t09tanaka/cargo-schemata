use std::sync::LazyLock;

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn double(a: u32) -> u32 {
    a * 2
}

pub fn base() -> u32 {
    40
}

/// The tests never go above 1000, so `>` and `>=` live; only the lazy tests reach it.
pub fn pad(x: u32) -> u32 {
    if x > 1000 { 1000 } else { x }
}

// Built by the first test that reads it; later tests find it done.
static TABLE: LazyLock<Vec<u32>> = LazyLock::new(|| vec![pad(base() + 2)]);

pub fn first() -> u32 {
    TABLE[0]
}

pub fn threaded_sum(a: u32, b: u32) -> u32 {
    std::thread::spawn(move || if a > 100 { 0 } else { a * 10 + b })
        .join()
        .unwrap()
}

pub fn never_called() -> u32 {
    7
}

/// The tests never use it above 100, so `>` and `>=` live.
pub fn clamp(x: u32) -> u32 {
    if x > 100 { 100 } else { x }
}

pub fn gate() -> bool {
    true
}

/// A unit test catches its mutants at once; `late_slow` in `tests/other.rs`
/// takes 2 s and passes whatever it returns.
pub fn late() -> u32 {
    5
}

/// The other way round: the unit test is the slow one that does not look, and
/// `early_is_six` in `tests/other.rs` catches its mutants at once.
pub fn early() -> u32 {
    6
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn alpha_add() {
        assert_eq!(add(1, 2), 3);
    }

    #[test]
    fn beta_double() {
        assert_eq!(double(4), 8);
    }

    // does not look at what it gets
    #[test]
    fn lazy_warm_up() {
        let _ = first();
    }

    #[test]
    fn lazy_reader() {
        assert_eq!(first(), 42);
    }

    #[test]
    fn threaded() {
        assert_eq!(threaded_sum(2, 3), 23);
    }

    #[test]
    fn clamp_small() {
        assert_eq!(clamp(5), 5);
    }

    #[test]
    fn gate_fast() {
        assert!(gate());
    }

    // the one slow test; it does not touch `clamp`, `add` or `double`
    #[test]
    fn gate_slow() {
        std::thread::sleep(Duration::from_secs(3));
        assert!(gate());
    }

    #[test]
    fn late_is_five() {
        assert_eq!(late(), 5);
    }

    // slow, and passes whatever `early` returns
    #[test]
    fn early_slow() {
        std::thread::sleep(Duration::from_secs(2));
        let _ = early();
    }
}

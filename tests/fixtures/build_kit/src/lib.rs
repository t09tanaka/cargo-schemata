//! A small crate whose mutants exercise every way the build stage can treat them.
#![deny(warnings)]

use std::fmt;
use std::ops::{Add, Sub};

mod inner;
pub use inner::double;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P {
    pub n: i32,
}

impl Add for P {
    type Output = P;
    fn add(self, other: P) -> P {
        P { n: self.n + other.n }
    }
}

/// Subtraction gives a plain number, so `a + b` and `a - b` have different types.
impl Sub for P {
    type Output = i32;
    fn sub(self, other: P) -> i32 {
        self.n - other.n
    }
}

impl fmt::Display for P {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.n)
    }
}

/// `a + b` -> `a - b` changes the type but still prints; `a * b` does not compile.
pub fn sum_text(a: P, b: P) -> String {
    let s = a + b;
    format!("<{s}>")
}

/// Const context: the operator cannot be switched at run time.
pub const LIMIT: usize = 2 + 3;

pub fn limit() -> usize {
    LIMIT
}

/// The `&&` joining a `let` is part of a let chain.
pub fn both(a: Option<u32>, flag: bool) -> bool {
    if let Some(x) = a
        && x > 1
        && flag
    {
        true
    } else {
        false
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Cfg {
    pub a: u32,
    pub b: u32,
}

pub fn cfg() -> Cfg {
    Cfg {
        a: 1,
        ..Default::default()
    }
}

pub fn name(n: u8) -> &'static str {
    match n {
        1 => "one",
        2 => "two",
        _ => "many",
    }
}

pub fn classify(n: i32) -> &'static str {
    match n {
        x if x < 0 => "neg",
        0 => "zero",
        _ => "pos",
    }
}

/// Mutants inside the operands of other mutants.
pub fn poly(a: i32, b: i32, c: i32) -> i32 {
    (a + b) * (c - a)
}

pub fn evens(limit: u32) -> impl Iterator<Item = u32> {
    (0..limit).filter(|n| n % 2 == 0)
}

pub fn parse_double(s: &str) -> Result<u32, std::num::ParseIntError> {
    let n: u32 = s.trim().parse()?;
    Ok(n * 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_poly() {
        assert_eq!(poly(1, 2, 4), 9);
    }
}

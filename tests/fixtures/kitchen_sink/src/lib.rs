//! Fixture covering the kinds of mutants and the exclusion rules.
//!
//! The crate is only parsed by the listing tools, never compiled.

pub mod nested;
pub mod ops;
mod crlf;
mod dangling;

#[path = "renamed_file.rs"]
mod renamed;

#[cfg(test)]
mod tests {
    fn helper() -> bool {
        1 + 1 == 2
    }
}

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::num::{NonZeroI32, NonZeroU32};
use std::sync::{Arc, Mutex};

pub fn unit_fn() {
    println!("hello");
}

pub fn returns_bool() -> bool {
    true
}

pub fn returns_string(a: &str) -> String {
    a.to_uppercase()
}

pub fn returns_str() -> &'static str {
    "name"
}

pub fn returns_unsigned(a: u32) -> u32 {
    a.pow(2)
}

pub fn returns_signed(a: i64) -> i64 {
    a.abs()
}

pub fn returns_float(a: f64) -> f64 {
    a.sqrt()
}

pub fn returns_nonzero(a: u32) -> NonZeroU32 {
    NonZeroU32::new(a).unwrap()
}

pub fn returns_nonzero_signed(a: i32) -> NonZeroI32 {
    NonZeroI32::new(a).unwrap()
}

pub fn returns_option(a: u8) -> Option<u8> {
    a.checked_add(1)
}

pub fn returns_option_string(a: &str) -> Option<String> {
    a.strip_prefix("x").map(str::to_owned)
}

pub fn returns_result(a: &str) -> Result<u32, std::num::ParseIntError> {
    a.parse()
}

pub fn returns_fmt_result(f: &mut std::fmt::Formatter) -> std::fmt::Result {
    f.write_str("x")
}

pub fn returns_vec(a: u8) -> Vec<u8> {
    vec![a, a]
}

pub fn returns_cow(a: &str) -> Cow<'static, str> {
    Cow::Owned(a.to_owned())
}

pub fn returns_boxed(a: u8) -> Box<u8> {
    Box::new(a)
}

pub fn returns_arc_mutex(a: u8) -> Arc<Mutex<u8>> {
    Arc::new(Mutex::new(a))
}

pub fn returns_set(a: u8) -> BTreeSet<u8> {
    BTreeSet::from([a])
}

pub fn returns_deque(a: u8) -> VecDeque<u8> {
    VecDeque::from([a])
}

pub fn returns_hash_map(a: u8) -> HashMap<String, bool> {
    HashMap::from([(a.to_string(), a > 1)])
}

pub fn returns_btree_map(a: u8) -> BTreeMap<u8, u8> {
    BTreeMap::from([(a, a)])
}

pub fn returns_tuple(a: u8) -> (u8, bool) {
    (a, a > 3)
}

pub fn returns_array(a: u8) -> [u8; 3] {
    [a; 3]
}

pub fn returns_slice(a: &[u8]) -> &[u8] {
    &a[1..]
}

pub fn returns_ref(a: &u8) -> &u8 {
    a
}

pub fn returns_iterator(a: u8) -> impl Iterator<Item = u8> {
    0..a
}

pub fn returns_other_impl(a: u8) -> impl std::fmt::Display {
    a
}

pub fn returns_never() -> ! {
    panic!("never returns")
}

pub fn returns_custom(a: u8) -> Custom {
    Custom { a }
}

pub fn returns_generic_wrapper(a: u8) -> Wrapper<u8> {
    Wrapper(a)
}

pub struct Custom {
    a: u8,
}

pub struct Wrapper<T>(T);

pub fn body_equals_replacement() -> bool {
    true
}

pub fn body_equals_unit() {
    ()
}

pub fn empty_body() {}

pub fn empty_body_with_return() -> u32 {}

pub unsafe fn unsafe_fn(p: *const u8) -> u8 {
    unsafe { *p + 1 }
}

#[test]
fn a_test() {
    assert!(1 + 1 == 2);
}

#[tokio::test]
async fn an_async_test() {
    assert!(1 + 1 == 2);
}

#[cfg(test)]
fn test_only_helper() -> u8 {
    1 + 2
}

#[mutants::skip]
pub fn skipped_by_attribute(a: u8) -> u8 {
    a + 1
}

#[cfg_attr(test, mutants::skip)]
pub fn skipped_by_cfg_attr(a: u8) -> u8 {
    a + 1
}

#[mutants::skip]
pub mod skipped_module {
    pub fn inside(a: u8) -> u8 {
        a + 1
    }
}

pub fn calls_with_capacity(n: usize) -> usize {
    let v: Vec<u8> = Vec::with_capacity(n + 1);
    v.capacity() * 2
}

pub fn method_with_capacity(n: usize) -> usize {
    let s = String::with_capacity(n * 2);
    s.len() - 1
}

pub fn unicode_before_operator(a: u32) -> String {
    let arrow = "é→é";
    format!("{arrow}{}", a + 1)
}

pub fn nested_functions(a: u32) -> u32 {
    fn inner(b: u32) -> u32 {
        b * 3
    }
    let closure = |c: u32| c - 1;
    inner(a) + closure(a)
}

pub fn multi_line_expression(a: u32, b: u32) -> bool {
    a > 1
        && b < 2
        || a
            == b
}

pub fn assignments(mut a: u32, b: u32) -> u32 {
    a += b;
    a -= b;
    a *= b;
    a /= b;
    a %= b;
    a <<= 1;
    a >>= 1;
    a &= b;
    a |= b;
    a ^= b;
    a
}

pub fn arithmetic(a: u32, b: u32) -> u32 {
    (a + b) - (a * b) / (a % b)
}

pub fn shifts_and_bits(a: u32, b: u32) -> u32 {
    (a << b) | (a >> b) & (a ^ b)
}

pub fn comparisons(a: u32, b: u32) -> bool {
    a == b || a != b || a < b || a <= b || a > b || a >= b
}

pub fn unary(a: i32, b: bool) -> bool {
    !b && -a < 0
}

pub fn deref_and_ref(a: &bool) -> bool {
    *a && !*a
}

pub fn match_with_catchall(a: u8) -> &'static str {
    match a {
        0 => "zero",
        1 | 2 => "small",
        n if n > 100 => "huge",
        n if n % 2 == 0 => "even",
        _ => "other",
    }
}

pub fn match_without_catchall(a: Option<u8>) -> u8 {
    match a {
        Some(n) if n > 3 => n,
        Some(n) => n + 1,
        None => 0,
    }
}

pub fn match_multiline_pattern(a: &str) -> u8 {
    match a {
        "alpha"
        | "beta" => 1,
        Some_Other => 2,
        _ => 3,
    }
}

#[derive(Default)]
pub struct Config {
    pub name: String,
    pub retries: u32,
    pub verbose: bool,
}

pub fn struct_with_base(name: String) -> Config {
    Config {
        name,
        retries: 3,
        verbose: true,
        ..Default::default()
    }
}

pub fn struct_without_base(name: String) -> Config {
    Config {
        name,
        retries: 3,
        verbose: true,
    }
}

pub fn struct_base_no_trailing_comma(name: String) -> Config {
    Config { name, retries: 1, ..Config::default() }
}

pub fn if_let_chain(a: Option<u32>, b: bool) -> u32 {
    if let Some(x) = a && x > 1 && b {
        x + 1
    } else {
        0
    }
}

pub fn while_let_chain(mut a: Vec<u32>) -> u32 {
    let mut total = 0;
    while let Some(x) = a.pop() && x != 0 {
        total += x;
    }
    total
}

pub fn plain_and_inside_if(a: bool, b: bool) -> bool {
    if a && b { a || b } else { false }
}

pub const fn const_function(a: u32) -> u32 {
    a * 2 + 1
}

pub const LIMIT: u32 = 10 * 2 - 1;

pub static TABLE_SIZE: usize = 4 + 4;

pub const BOOL_CONST: bool = !false && LIMIT > 3;

pub const NESTED_FN_IN_CONST: u32 = {
    fn helper(a: u32) -> u32 {
        a + 1
    }
    helper(2) * 3
};

pub struct Buffers {
    pub small: [u8; LIMIT as usize + 1],
    pub grid: [[u8; 2 * 2]; 3],
}

pub fn array_types_and_repeat(n: u32) -> u32 {
    let local: [u8; 2 + 2] = [0; 1 + 3];
    let _unused = local;
    n + 1
}

pub struct Generic<const N: usize>;

pub fn const_generic_argument() -> Generic<{ 2 * 3 }> {
    Generic::<{ 1 + 1 }>
}

pub enum Discriminants {
    A = 1 << 2,
    B = 4 + 1,
    C,
}

pub fn const_block(a: u32) -> u32 {
    let k = const { 3 * 4 };
    a + k
}

pub struct Holder;

impl Holder {
    pub const SCALE: u32 = 6 * 7;

    pub fn new(a: u32) -> Self {
        let _ = a + 1;
        Holder
    }

    pub fn build(a: u32) -> u32 {
        a * Self::SCALE
    }

    pub const fn const_method(a: u32) -> u32 {
        a - 1
    }

    pub unsafe fn unsafe_method(&self) -> u32 {
        1 + 1
    }

    #[mutants::skip]
    pub fn skipped_method(&self) -> u32 {
        1 + 1
    }

    pub fn nested_return(&self) -> Vec<Option<String>> {
        Vec::new()
    }
}

pub trait Shape {
    const SIDES: u32 = 2 + 2;

    fn area(&self) -> f64;

    fn describe(&self) -> String {
        format!("{}", self.area() * 2.0)
    }

    fn new() -> Self
    where
        Self: Sized,
    {
        unimplemented!()
    }

    fn empty_default(&self) {}
}

impl Shape for Holder {
    const SIDES: u32 = 1 + 1;

    fn area(&self) -> f64 {
        1.0 + 2.0
    }
}

impl<T: Clone> Shape for Wrapper<T> {
    fn area(&self) -> f64 {
        3.0 * 4.0
    }
}

impl<'a, T> Shape for &'a [T] {
    fn area(&self) -> f64 {
        self.len() as f64 - 1.0
    }
}

impl Default for Holder {
    fn default() -> Self {
        let _ = 1 + 1;
        Holder
    }
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", 1 + 1)
    }
}

#[mutants::skip]
impl Shape for Custom {
    fn area(&self) -> f64 {
        1.0 + 1.0
    }
}

#[mutants::skip]
pub trait SkippedTrait {
    fn provided(&self) -> u32 {
        1 + 1
    }
}

pub mod inline {
    pub fn in_inline_module(a: u32) -> u32 {
        a + 1
    }

    pub mod deeper {
        pub fn in_deeper_module(a: u32) -> bool {
            a > 1
        }
    }
}

macro_rules! not_visited {
    ($a:expr) => {
        $a + 1
    };
}

pub fn uses_macro(a: u32) -> u32 {
    not_visited!(a) + 2
}

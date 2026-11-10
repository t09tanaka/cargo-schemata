//! A crate whose test records the `TMPDIR` it runs with and leaves a read-only
//! directory in it, as a test that is killed can.

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

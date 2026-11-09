//! A file module declared with `pub mod ops;` in the library root.

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn is_even(a: u32) -> bool {
    a % 2 == 0
}

pub struct Counter {
    pub n: u32,
}

impl Counter {
    pub fn bump(&mut self) {
        self.n += 1;
    }

    pub fn is_zero(&self) -> bool {
        self.n == 0
    }
}

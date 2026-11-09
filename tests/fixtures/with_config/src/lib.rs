pub mod generated;
pub mod sub;

pub enum Error {
    Mutated,
    Real,
}

pub fn check(a: u32) -> Result<u32, Error> {
    if a == 3 { Err(Error::Real) } else { Ok(a + 1) }
}

pub fn sums(a: u32, b: u32) -> u32 {
    let width = Vec::<u8>::with_capacity(a as usize + 1).len() as u32;
    trace_call(a - 1);
    log(b * 2);
    width + a - b
}

pub fn negate(a: i32) -> i32 {
    -a
}

pub fn eq(a: u32, b: u32) -> bool {
    a == b
}

fn trace_call(_a: u32) {}
fn log(_a: u32) {}

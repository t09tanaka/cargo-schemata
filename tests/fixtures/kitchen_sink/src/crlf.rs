//! This file has CRLF line endings on purpose.

pub fn crlf_fn(a: u32) -> u32 {
    let label = "é→é";
    if a > 1 { a + label.len() as u32 } else { 0 }
}

pub fn crlf_match(a: u8) -> u8 {
    match a {
        0 => 1,
        1 => 2,
        _ => 3,
    }
}

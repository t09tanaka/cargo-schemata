pub fn deep(a: u32) -> bool {
    a >= 10
}

#[cfg(test)]
mod tests {
    #[test]
    fn deep_works() {
        assert!(super::deep(11));
    }
}

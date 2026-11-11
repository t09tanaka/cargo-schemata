// A second test binary. For `late` it has the slow test that never fails, for
// `early` the quick one that does: whichever binary comes first, one of the two
// functions has its quick test in the other.
#[test]
fn late_slow() {
    std::thread::sleep(std::time::Duration::from_secs(2));
    let _ = route_kit::late();
}

#[test]
fn early_is_six() {
    assert_eq!(route_kit::early(), 6);
}

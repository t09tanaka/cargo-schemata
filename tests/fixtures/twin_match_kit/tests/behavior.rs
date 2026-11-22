use twin_match_kit::*;

#[test]
fn picks() {
    assert_eq!(earlier(Some(3), false), 3);
    assert_eq!(later(None, false), 9);
}

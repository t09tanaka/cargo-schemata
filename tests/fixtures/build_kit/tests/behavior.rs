use build_kit::*;

#[test]
fn sum_text_adds() {
    assert_eq!(sum_text(P { n: 1 }, P { n: 2 }), "<3>");
}

#[test]
fn limit_is_five() {
    assert_eq!(limit(), 5);
}

#[test]
fn both_needs_everything() {
    assert!(both(Some(2), true));
    assert!(!both(Some(1), true));
    assert!(!both(Some(2), false));
    assert!(!both(None, true));
}

#[test]
fn cfg_keeps_a() {
    assert_eq!(cfg(), Cfg { a: 1, b: 0 });
}

#[test]
fn names() {
    assert_eq!(name(1), "one");
    assert_eq!(name(2), "two");
    assert_eq!(name(3), "many");
}

#[test]
fn classes() {
    assert_eq!(classify(-1), "neg");
    assert_eq!(classify(0), "zero");
    assert_eq!(classify(5), "pos");
}

#[test]
fn polynomial() {
    assert_eq!(poly(1, 2, 4), 9);
}

#[test]
fn evens_are_even() {
    assert_eq!(evens(7).collect::<Vec<_>>(), vec![0, 2, 4, 6]);
}

#[test]
fn parses_and_doubles() {
    assert_eq!(parse_double(" 21 ").unwrap(), 42);
    assert!(parse_double("x").is_err());
    assert_eq!(double(4), 8);
}

#[test]
fn manifest_dir_is_set_at_run_time() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    assert!(std::path::Path::new(&dir).join("Cargo.toml").is_file());
}

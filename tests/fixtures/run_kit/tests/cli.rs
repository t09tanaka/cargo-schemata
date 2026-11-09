//! Only the binary built together with the tests tells whether `limit` is right.

#[test]
fn binary_prints_the_limit() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_run-kit"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "10");
}

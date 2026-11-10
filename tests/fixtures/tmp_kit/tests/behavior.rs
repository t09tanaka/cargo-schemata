use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn add_is_exact() {
    assert_eq!(tmp_kit::add(2, 3), 5);
    assert_eq!(tmp_kit::add(0, 7), 7);

    let (Some(record), Some(tmp)) = (
        std::env::var_os("TMP_KIT_RECORD"),
        std::env::var_os("TMPDIR"),
    ) else {
        return;
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record)
        .unwrap();
    writeln!(file, "{}", Path::new(&tmp).display()).unwrap();
    // what a test that is killed in the middle leaves behind
    let dir = Path::new(&tmp).join("left-read-only");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), "x").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
}

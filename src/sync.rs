//! Keep a work directory in step with the source tree, touching only what differs.
//!
//! The work directory has a fixed path so that Cargo's incremental state
//! survives between runs: files that did not change keep their modification
//! times and are not rebuilt.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Replacement contents for files, keyed by path relative to the tree root.
pub type Overrides = HashMap<PathBuf, String>;

/// Make `dst` a copy of `src`, except that `overrides` give the contents of
/// some files. `.git` and `target` at the root, and the `exclude` paths, are
/// not copied.
pub fn sync_tree(src: &Path, dst: &Path, exclude: &[PathBuf], overrides: &Overrides) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    sync_dir(src, dst, Path::new(""), exclude, overrides)
}

/// Whether an entry of the tree is not copied: `.git` and `target` at the
/// root (`rel_dir` is the directory of the entry, relative to the root), and
/// the `exclude` paths. What `--trace-only` digests follows the same rule.
pub fn left_out(
    rel_dir: &Path,
    name: &std::ffi::OsStr,
    src_path: &Path,
    exclude: &[PathBuf],
) -> bool {
    rel_dir.as_os_str().is_empty() && (name == ".git" || name == "target")
        || exclude.iter().any(|e| e == src_path)
}

fn sync_dir(
    src_root: &Path,
    dst_root: &Path,
    rel: &Path,
    exclude: &[PathBuf],
    overrides: &Overrides,
) -> Result<()> {
    let src_dir = src_root.join(rel);
    let dst_dir = dst_root.join(rel);
    let mut wanted: HashSet<std::ffi::OsString> = HashSet::new();
    for entry in fs::read_dir(&src_dir).with_context(|| format!("read {}", src_dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let rel_path = rel.join(&name);
        let src_path = entry.path();
        if left_out(rel, &name, &src_path, exclude) {
            continue;
        }
        let meta = fs::symlink_metadata(&src_path)?;
        let dst_path = dst_dir.join(&name);
        if meta.is_dir() {
            wanted.insert(name);
            if dst_path.symlink_metadata().is_ok_and(|m| !m.is_dir()) {
                fs::remove_file(&dst_path)?;
            }
            fs::create_dir_all(&dst_path)?;
            sync_dir(src_root, dst_root, &rel_path, exclude, overrides)?;
        } else if let Some(text) = overrides.get(&rel_path) {
            wanted.insert(name);
            write_if_changed(&dst_path, text)?;
        } else if meta.file_type().is_symlink() {
            wanted.insert(name);
            let target = fs::read_link(&src_path)?;
            if fs::read_link(&dst_path).ok().as_ref() != Some(&target) {
                remove_any(&dst_path)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &dst_path)?;
            }
        } else {
            wanted.insert(name);
            let mtime = meta.modified()?;
            let same = dst_path.symlink_metadata().ok().is_some_and(|d| {
                d.is_file() && d.len() == meta.len() && d.modified().ok() == Some(mtime)
            });
            if !same {
                remove_any(&dst_path)?;
                fs::copy(&src_path, &dst_path)
                    .with_context(|| format!("copy {}", src_path.display()))?;
                let f = fs::OpenOptions::new().write(true).open(&dst_path)?;
                f.set_modified(mtime)?;
            }
        }
    }
    if let Ok(read) = fs::read_dir(&dst_dir) {
        for entry in read {
            let entry = entry?;
            if !wanted.contains(&entry.file_name()) {
                remove_any(&entry.path())?;
            }
        }
    }
    Ok(())
}

fn remove_any(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(_) => {}
    }
    Ok(())
}

/// Write `text` unless the file already has exactly that content.
pub fn write_if_changed(path: &Path, text: &str) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(());
    }
    remove_if_symlink(path)?;
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn remove_if_symlink(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("schemata-sync-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copies_overrides_and_removes_extras() {
        let root = tmp("a");
        let (src, dst) = (root.join("src"), root.join("dst"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(src.join("target")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("sub/b.txt"), "b").unwrap();
        fs::write(src.join("target/x"), "x").unwrap();
        sync_tree(&src, &dst, &[], &Overrides::new()).unwrap();
        assert_eq!(fs::read_to_string(dst.join("sub/b.txt")).unwrap(), "b");
        assert!(!dst.join("target").exists());
        fs::write(dst.join("stale"), "s").unwrap();
        let mut ov = Overrides::new();
        ov.insert(PathBuf::from("a.txt"), "generated".into());
        sync_tree(&src, &dst, &[], &ov).unwrap();
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "generated");
        assert!(!dst.join("stale").exists());
        // back to the original content when the override goes away
        sync_tree(&src, &dst, &[], &Overrides::new()).unwrap();
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "a");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unchanged_files_keep_their_mtime() {
        let root = tmp("b");
        let (src, dst) = (root.join("src"), root.join("dst"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        sync_tree(&src, &dst, &[], &Overrides::new()).unwrap();
        let m1 = fs::metadata(dst.join("a.txt")).unwrap().modified().unwrap();
        sync_tree(&src, &dst, &[], &Overrides::new()).unwrap();
        let m2 = fs::metadata(dst.join("a.txt")).unwrap().modified().unwrap();
        assert_eq!(m1, m2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_override_wins_over_a_symlink() {
        let root = tmp("c");
        let (src, dst) = (root.join("src"), root.join("dst"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real.txt"), "real").unwrap();
        std::os::unix::fs::symlink("real.txt", src.join("link.txt")).unwrap();
        // first without, so that dst holds a symlink, then with the override
        sync_tree(&src, &dst, &[], &Overrides::new()).unwrap();
        assert!(
            dst.join("link.txt")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        let mut ov = Overrides::new();
        ov.insert(PathBuf::from("link.txt"), "generated".into());
        sync_tree(&src, &dst, &[], &ov).unwrap();
        assert_eq!(
            fs::read_to_string(dst.join("link.txt")).unwrap(),
            "generated"
        );
        assert!(
            !dst.join("link.txt")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(dst.join("real.txt")).unwrap(), "real");
        let _ = fs::remove_dir_all(&root);
    }
}

// Copyright 2023-2025 Martin Pool
// Ported from cargo-mutants 27.1.0 (MIT); see NOTICE.

//! Find the cargo workspace and the packages whose sources are examined.

use std::panic::catch_unwind;
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::TargetKind;
use itertools::Itertools;
use serde_json::Value;

/// A package built and tested as a unit.
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct Package {
    /// The short name of the package.
    pub name: String,
    /// The directory for this package relative to the workspace (`""` for the root).
    pub relative_dir: Utf8PathBuf,
    /// The root source files of the mutated targets, relative to the workspace root.
    pub top_sources: Vec<Utf8PathBuf>,
}

impl Package {
    fn from_cargo_metadata(
        package_metadata: &cargo_metadata::Package,
        workspace_root: &Utf8Path,
    ) -> Option<Self> {
        let relative_dir = package_metadata
            .manifest_path
            .strip_prefix(workspace_root)
            .ok()
            .and_then(|p| p.parent())
            .map(ToOwned::to_owned)?;
        Some(Package {
            name: package_metadata.name.to_string(),
            top_sources: package_top_sources(workspace_root, package_metadata),
            relative_dir,
        })
    }
}

/// Find all the files that are named in the `path` of targets in a
/// Cargo manifest, if the kind of the target is one that we should mutate.
fn package_top_sources(
    workspace_root: &Utf8Path,
    package_metadata: &cargo_metadata::Package,
) -> Vec<Utf8PathBuf> {
    let mut found = Vec::new();
    for target in &package_metadata.targets {
        if should_mutate_target(target)
            && let Ok(relpath) = target.src_path.strip_prefix(workspace_root)
        {
            found.push(relpath.to_owned());
        }
    }
    found.sort();
    found.dedup();
    found
}

fn should_mutate_target(target: &cargo_metadata::Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Bin
                | TargetKind::ProcMacro
                | TargetKind::CDyLib
                | TargetKind::DyLib
                | TargetKind::Lib
                | TargetKind::RLib
                | TargetKind::StaticLib
        )
    })
}

/// Which packages of a workspace to examine.
#[derive(Debug, Clone)]
pub enum PackageFilter {
    /// Every package in the workspace.
    All,
    /// Packages named explicitly.
    Explicit(Vec<String>),
    /// The package containing this directory, or the default packages of the
    /// workspace if it is the workspace root.
    Auto(Utf8PathBuf),
}

enum PackageSelection {
    All,
    Explicit(Vec<Arc<Package>>),
}

pub struct Workspace {
    metadata: cargo_metadata::Metadata,
    packages: Vec<Arc<Package>>,
}

impl Workspace {
    pub fn root(&self) -> &Utf8Path {
        &self.metadata.workspace_root
    }

    pub fn open(start_dir: &Utf8Path) -> Result<Self> {
        let dir = locate_project(start_dir, true)?;
        let manifest_path = dir.join("Cargo.toml");
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .manifest_path(&manifest_path)
            .current_dir(&dir)
            .verbose(false)
            .exec()
            .with_context(|| format!("Failed to run cargo metadata on {manifest_path}"))?;
        let packages = metadata
            .workspace_packages()
            .into_iter()
            .sorted_by_key(|p| &p.name)
            .filter_map(|p| Package::from_cargo_metadata(p, &metadata.workspace_root))
            .map(Arc::new)
            .collect();
        Ok(Workspace { metadata, packages })
    }

    fn packages_by_name<S: AsRef<str>>(&self, names: &[S]) -> Vec<Arc<Package>> {
        names
            .iter()
            .map(AsRef::as_ref)
            .sorted()
            .filter_map(|name| {
                let found = self.packages.iter().find(|p| p.name == name);
                if found.is_none() {
                    eprintln!("warning: package {name:?} not found in source tree");
                }
                found.map(Arc::clone)
            })
            .collect()
    }

    fn filter_packages(&self, filter: &PackageFilter) -> Result<PackageSelection> {
        match filter {
            PackageFilter::Auto(dir) => {
                let root = self.root();
                let package_dir = locate_project(dir, false)?;
                for package in &self.packages {
                    if root.join(&package.relative_dir) == package_dir {
                        return Ok(PackageSelection::Explicit(vec![package.clone()]));
                    }
                }
                ensure!(
                    package_dir == root,
                    "package {package_dir:?} doesn't match any child and doesn't match the workspace root {root:?}?",
                );
                Ok(self.default_packages())
            }
            PackageFilter::All => Ok(PackageSelection::All),
            PackageFilter::Explicit(names) => {
                Ok(PackageSelection::Explicit(self.packages_by_name(names)))
            }
        }
    }

    /// The packages selected by `filter`.
    pub fn select(&self, filter: &PackageFilter) -> Result<Vec<Arc<Package>>> {
        Ok(match self.filter_packages(filter)? {
            PackageSelection::All => self.packages.clone(),
            PackageSelection::Explicit(packages) => packages,
        })
    }

    fn default_packages(&self) -> PackageSelection {
        let metadata = &self.metadata;
        match catch_unwind(|| metadata.workspace_default_packages()) {
            Ok(default_packages) => {
                let names: Vec<&str> = default_packages
                    .iter()
                    .map(|p| p.name.as_str())
                    .sorted()
                    .collect();
                PackageSelection::Explicit(self.packages_by_name(&names))
            }
            Err(_) => PackageSelection::All,
        }
    }
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Run `cargo locate-project` and return the directory of the (workspace) manifest.
fn locate_project(path: &Utf8Path, workspace: bool) -> Result<Utf8PathBuf> {
    ensure!(path.is_dir(), "{path:?} is not a directory");
    let mut args: Vec<&str> = vec!["locate-project"];
    if workspace {
        args.push("--workspace");
    }
    let output = Command::new(cargo_bin())
        .args(&args)
        .current_dir(path)
        .output()
        .with_context(|| format!("failed to spawn {args:?}"))?;
    if !output.status.success() {
        bail!(
            "cargo locate-project failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout =
        String::from_utf8(output.stdout).context("cargo locate-project output is not UTF-8")?;
    let val: Value = serde_json::from_str(&stdout).context("parse cargo locate-project output")?;
    let cargo_toml_path: Utf8PathBuf = val["root"]
        .as_str()
        .with_context(|| format!("cargo locate-project output has no root: {stdout:?}"))?
        .to_owned()
        .into();
    ensure!(
        cargo_toml_path.is_file(),
        "cargo locate-project root {cargo_toml_path:?} is not a file"
    );
    let root = cargo_toml_path
        .parent()
        .ok_or_else(|| anyhow!("cargo locate-project root {cargo_toml_path:?} has no parent"))?
        .to_owned();
    Ok(root)
}

//! Where built things live, and how they are named.
//!
//! A package is installed at the path it will be used from, so nothing is ever
//! relocated after the fact: `--prefix` is the store path, and the container
//! mounts that same path. A cross compiler that bakes its prefix into itself
//! therefore still works when something else depends on it.
//!
//! A store directory is only real once `.kb-ok` exists. It is written last, so
//! an interrupted build leaves a directory that is recognised as rubbish and
//! removed, never one that is mistaken for a finished package.

use crate::err::{Error, Result, bail};
use crate::recipe::{Kind, Recipe};
use crate::sha256::{self, Sha256};
use crate::target::Target;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Bumped when the engine changes how it builds, invalidating every store entry.
const ENGINE: &str = "kb-build-v6";

pub const MARKER: &str = ".kb-ok";

/// Where things are mounted inside the build container. Baked into binaries,
/// so it must not change without bumping `ENGINE`.
pub const C_STORE: &str = "/kb/store";
pub const C_WORK: &str = "/kb/work";
pub const C_SOURCES: &str = "/kb/sources";
pub const C_SYSROOT: &str = "/kb/sysroot";

/// Delete a tree that a build left behind.
///
/// `fs::remove_dir_all` cannot unlink a file out of a directory it may not
/// write to, and gcc's tarball ships several: `libffi`, `libphobos` and
/// `.forgejo` all unpack read-only. The engine used to ignore the error and
/// silently accumulate a two-gigabyte work tree per build.
pub fn remove_tree(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    // Best effort: if chmod is unavailable the remove below reports the real
    // problem, and there is nothing useful to say about chmod itself.
    let _ = std::process::Command::new("chmod")
        .arg("-R")
        .arg("u+rwX")
        .arg(dir)
        .status();
    fs::remove_dir_all(dir).map_err(|e| Error::new(format!("removing {}: {e}", dir.display())))
}

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn new(build_dir: &Path) -> Result<Store> {
        let root = build_dir.join("store");
        fs::create_dir_all(&root)?;
        Ok(Store { root })
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub fn is_built(&self, id: &str) -> bool {
        self.path(id).join(MARKER).is_file()
    }

    /// Clear the way for a build: a directory without the marker is the
    /// wreckage of an interrupted one.
    pub fn prepare(&self, id: &str) -> Result<PathBuf> {
        let dir = self.path(id);
        remove_tree(&dir)?;
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn finalize(&self, id: &str, note: &str) -> Result<()> {
        fs::write(self.path(id).join(MARKER), note)?;
        Ok(())
    }

    /// Throw away a failed build. The caller is already reporting a failure,
    /// so a cleanup problem is worth a word but must not replace it.
    pub fn discard(&self, id: &str) {
        if let Err(e) = remove_tree(&self.path(id)) {
            eprintln!("kb: could not clean up after a failed build: {e}");
        }
    }
}

/// The identity of a build: same inputs, same id, same directory.
pub fn build_id(recipe: &Recipe, target: &Target, dep_ids: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(ENGINE.as_bytes());
    h.update(b"\ntarget\n");
    h.update(target.build_identity().as_bytes());
    h.update(b"\nrecipe\n");
    h.update(recipe.name.as_bytes());
    h.update(b"\n");
    h.update(recipe.version.as_bytes());
    h.update(b"\n");
    h.update(sha256::digest(&recipe.raw).as_bytes());
    if recipe.reads_kernel_config() {
        h.update(b"\nkernel-config\n");
        h.update(target.kernel_identity().as_bytes());
    }
    h.update(b"\ndeps\n");
    // BTreeMap iterates sorted, so the id does not depend on discovery order.
    for (name, id) in dep_ids {
        h.update(name.as_bytes());
        h.update(b"=");
        h.update(id.as_bytes());
        h.update(b"\n");
    }
    let digest = sha256::hex(&h.finish());
    format!("{}-{}-{}", &digest[..16], recipe.name, recipe.version)
}

/// A sysroot is the merged install trees of every `target`-kind dependency.
///
/// It is a copy, not a symlink or hardlink farm: a build that writes through a
/// link would reach into the store and corrupt a package it does not own. The
/// copy costs disk, which the post-mortem measured as the resource we have.
pub fn sysroot(build_dir: &Path, store: &Store, dep_ids: &[(&str, Kind, String)]) -> Result<PathBuf> {
    let targets: Vec<&String> = dep_ids
        .iter()
        .filter(|(_, kind, _)| *kind == Kind::Target)
        .map(|(_, _, id)| id)
        .collect();

    let mut h = Sha256::new();
    for id in &targets {
        h.update(id.as_bytes());
        h.update(b"\n");
    }
    let key = &sha256::hex(&h.finish())[..16];
    let dir = build_dir.join("sysroot").join(key);

    if dir.join(MARKER).is_file() {
        return Ok(dir);
    }
    remove_tree(&dir)?;
    fs::create_dir_all(&dir)?;

    for id in &targets {
        let from = store.path(id);
        let status = std::process::Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", from.display()))
            .arg(&dir)
            .status()?;
        if !status.success() {
            bail!("failed to merge {} into the sysroot", from.display());
        }
    }
    fs::write(dir.join(MARKER), targets.iter().fold(String::new(), |mut s, id| {
        s.push_str(id);
        s.push('\n');
        s
    }))?;
    Ok(dir)
}

//! Assembling an image: the store entries a target names, merged into one
//! rootfs, plus the initramfs the kernel unpacks.
//!
//! What varies per target is the `contents` list and nothing else. If this
//! file ever needs to know which architecture it is assembling, Gate 0
//! criterion 3 has already failed.

use crate::build::Engine;
use crate::cpio;
use crate::err::{Error, Result, bail};
use crate::recipe::{Kind, Recipe};
use crate::store;
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// Not in an image: development files, documentation and translations. An
/// image is what runs, and the rest is what a build machine keeps.
const PRUNE_DIRS: &[&str] = &[
    "usr/include",
    "usr/share/man",
    "usr/share/info",
    "usr/share/doc",
    "usr/share/locale",
    "usr/share/i18n",
    "usr/lib/pkgconfig",
    "usr/share/pkgconfig",
    "usr/share/aclocal",
];
const PRUNE_SUFFIXES: &[&str] = &[".a", ".la", ".o", ".pc"];

pub struct Image {
    pub dir: PathBuf,
    pub rootfs: PathBuf,
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    pub manifest: PathBuf,
}

impl Image {
    pub fn of(engine: &Engine, target: &Target) -> Image {
        let dir = engine.build_dir.join("images").join(&target.name);
        Image {
            rootfs: dir.join("rootfs"),
            kernel: dir.join("vmlinuz"),
            initramfs: dir.join("initramfs.cpio"),
            manifest: dir.join("manifest.tsv"),
            dir,
        }
    }

    pub fn exists(&self) -> bool {
        self.kernel.is_file() && self.initramfs.is_file() && self.manifest.is_file()
    }
}

pub fn assemble(
    engine: &Engine,
    recipes: &BTreeMap<String, Recipe>,
    target: &Target,
) -> Result<Image> {
    if target.contents.is_empty() {
        bail!("targets/{}.toml: contents is empty, so there is no image to build", target.name);
    }
    let ids = engine.build_all(recipes, &target.contents, target)?;
    let order = runtime_closure(recipes, &target.contents)?;

    for name in &order {
        if recipes[name].kind != Kind::Target {
            bail!(
                "{name} is a host tool and cannot be in an image\n  \
                 it reached {} through a runtime dependency, which is a modelling error",
                target.name
            );
        }
    }

    let image = Image::of(engine, target);
    store::remove_tree(&image.dir)?;
    fs::create_dir_all(&image.rootfs)?;

    let mut manifest: BTreeMap<String, String> = BTreeMap::new();
    let mut shared = 0usize;
    for name in &order {
        let id = &ids[name];
        let from = engine.store.path(id);
        for path in files_under(&from)? {
            if path == store::MARKER {
                continue;
            }
            if manifest.insert(path, id.clone()).is_some() {
                shared += 1;
            }
        }
        copy_into(&from, &image.rootfs)?;
    }
    fs::remove_file(image.rootfs.join(store::MARKER)).ok();

    prune(&image.rootfs, &mut manifest)?;
    strip(engine, &image.rootfs, &ids, target)?;

    // The kernel is what boots the userland, not part of it: carrying it
    // inside its own initramfs would cost that many megabytes of guest RAM.
    let boot = image.rootfs.join("boot");
    let vmlinuz = boot.join("vmlinuz");
    if !vmlinuz.is_file() {
        bail!(
            "no {} after assembling {}\n  contents must include a recipe that installs a kernel",
            vmlinuz.display(),
            target.name
        );
    }
    fs::rename(&vmlinuz, &image.kernel)?;
    store::remove_tree(&boot)?;
    manifest.retain(|path, _| !path.starts_with("boot/"));

    write_manifest(&image.manifest, &manifest)?;
    let bytes = pack(&image)?;

    println!(
        "image {}: {} packages, {} files, {} MiB rootfs, {} MiB initramfs",
        target.name,
        order.len(),
        manifest.len(),
        tree_bytes(&image.rootfs)? / (1 << 20),
        bytes / (1 << 20),
    );
    if shared > 0 {
        println!("  note: {shared} path(s) are installed by more than one package");
    }
    Ok(image)
}

/// The packages an image is made of: what the target names, and everything
/// those need at runtime. Build dependencies stop here — that is the whole
/// reason the recipe format keeps the two lists apart.
fn runtime_closure(recipes: &BTreeMap<String, Recipe>, roots: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut done = BTreeSet::new();
    for root in roots {
        visit(recipes, root, "contents", &mut done, &mut out)?;
    }
    Ok(out)
}

fn visit(
    recipes: &BTreeMap<String, Recipe>,
    name: &str,
    referrer: &str,
    done: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    if !done.insert(name.to_string()) {
        return Ok(());
    }
    let Some(recipe) = recipes.get(name) else {
        bail!("`{referrer}` names `{name}`, which has no recipe");
    };
    for dep in &recipe.deps.runtime {
        visit(recipes, dep, name, done, out)?;
    }
    out.push(name.to_string());
    Ok(())
}

fn copy_into(from: &Path, to: &Path) -> Result<()> {
    let status = std::process::Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", from.display()))
        .arg(to)
        .status()?;
    if !status.success() {
        bail!("could not merge {} into the image", from.display());
    }
    Ok(())
}

fn prune(rootfs: &Path, manifest: &mut BTreeMap<String, String>) -> Result<()> {
    for dir in PRUNE_DIRS {
        let path = rootfs.join(dir);
        if path.exists() {
            store::remove_tree(&path)?;
        }
    }
    let mut dropped = Vec::new();
    for path in manifest.keys() {
        let pruned_dir = PRUNE_DIRS.iter().any(|d| path.starts_with(&format!("{d}/")));
        let pruned_file = PRUNE_SUFFIXES.iter().any(|s| path.ends_with(s));
        if pruned_dir || pruned_file {
            dropped.push(path.clone());
        }
    }
    for path in dropped {
        let full = rootfs.join(&path);
        if full.symlink_metadata().is_ok() {
            fs::remove_file(&full)?;
        }
        manifest.remove(&path);
    }
    Ok(())
}

/// Debug information is most of a binary and names every build path in it;
/// the spec ships no debug packages, so it comes off here, with the target's
/// own strip, inside the seed container like every other tool we run.
/// Relocatable objects (kernel modules) keep their symbols: the kernel needs
/// them to link.
fn strip(engine: &Engine, rootfs: &Path, ids: &BTreeMap<String, String>, target: &Target) -> Result<()> {
    let Some(binutils) = ids.get("binutils") else {
        bail!("no binutils in the build, so nothing can strip the image");
    };
    let script = format!(
        "set -euo pipefail\n\
         strip={store}/{binutils}/bin/{triple}-strip\n\
         find /kb/image -type f -size +64c | while read -r f; do\n\
           magic=$(od -An -N4 -tx1 \"$f\" | tr -d ' ')\n\
           [ \"$magic\" = 7f454c46 ] || continue\n\
           kind=$(od -An -j16 -N1 -tu1 \"$f\" | tr -d ' ')\n\
           case $kind in 2|3) \"$strip\" --strip-unneeded --preserve-dates \"$f\" ;; esac\n\
         done\n",
        store = store::C_STORE,
        triple = target.triple,
    );
    let status = std::process::Command::new("podman")
        .args(["run", "--rm", "--network=none", "-i"])
        .arg("-v")
        .arg(format!("{}:{}:ro", engine.store.root.display(), store::C_STORE))
        .arg("-v")
        .arg(format!("{}:/kb/image", rootfs.display()))
        .arg(&engine.seed)
        .args(["bash", "-s"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().expect("piped").write_all(script.as_bytes())?;
            child.wait()
        })
        .map_err(|e| Error::new(format!("running podman: {e}")))?;
    if !status.success() {
        bail!("stripping the image failed");
    }
    Ok(())
}

fn pack(image: &Image) -> Result<u64> {
    let file = fs::File::create(&image.initramfs)?;
    let mut archive = cpio::Archive::new(BufWriter::new(file));
    cpio::append_tree(&mut archive, &image.rootfs, "")?;
    // The two nodes userspace needs before it has mounted devtmpfs: without
    // console the kernel has nowhere to open PID 1's stdio, and without null
    // dinit cannot start a single service.
    archive.char_device("dev/console", 0o600, 5, 1)?;
    archive.char_device("dev/null", 0o666, 1, 3)?;
    archive.finish()
}

fn write_manifest(path: &Path, manifest: &BTreeMap<String, String>) -> Result<()> {
    let mut text = String::from("path\tbuild\n");
    for (file, id) in manifest {
        text.push_str(file);
        text.push('\t');
        text.push_str(id);
        text.push('\n');
    }
    fs::write(path, text)?;
    Ok(())
}

pub fn read_manifest(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(path)
        .map_err(|e| Error::new(format!("{}: {e}\n  run: kb image <target>", path.display())))?;
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        let Some((file, id)) = line.split_once('\t') else {
            bail!("{}:{}: expected `path<tab>build`", path.display(), n + 1);
        };
        out.insert(file.to_string(), id.to_string());
    }
    Ok(out)
}

/// Every non-directory under `root`, relative to it. Symlinks are entries in
/// their own right and are never followed.
pub fn files_under(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    walk(root, "", &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), &name, out)?;
        } else {
            out.push(name);
        }
    }
    Ok(())
}

fn tree_bytes(root: &Path) -> Result<u64> {
    let mut total = 0;
    for path in files_under(root)? {
        total += root.join(path).symlink_metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{Build, Deps, System};
    use std::path::PathBuf;

    fn recipe(name: &str, kind: Kind, runtime: &[&str]) -> Recipe {
        Recipe {
            name: name.into(),
            version: "1".into(),
            kind,
            source: None,
            build: Build {
                system: System::Shell,
                out_of_tree: false,
                configure: vec![],
                make: None,
                install: None,
                env: vec![],
                script: Some(String::new()),
            },
            deps: Deps {
                build: vec!["gcc".into()],
                runtime: runtime.iter().map(|s| s.to_string()).collect(),
            },
            path: PathBuf::from(format!("recipes/{name}.toml")),
            raw: Vec::new(),
        }
    }

    fn recipes(rs: Vec<Recipe>) -> BTreeMap<String, Recipe> {
        rs.into_iter().map(|r| (r.name.clone(), r)).collect()
    }

    #[test]
    fn an_image_takes_runtime_dependencies_and_leaves_build_ones() {
        let rs = recipes(vec![
            recipe("bash", Kind::Target, &["glibc"]),
            recipe("glibc", Kind::Target, &[]),
            recipe("gcc", Kind::HostTool, &[]),
        ]);
        let order = runtime_closure(&rs, &["bash".to_string()]).unwrap();
        assert_eq!(order, vec!["glibc", "bash"], "dependencies come first");
        assert!(!order.contains(&"gcc".to_string()), "a build dependency reached the image");
    }

    #[test]
    fn a_missing_package_names_who_asked_for_it() {
        let rs = recipes(vec![recipe("bash", Kind::Target, &["ghost"])]);
        let e = runtime_closure(&rs, &["bash".to_string()]).unwrap_err().to_string();
        assert_eq!(e, "`bash` names `ghost`, which has no recipe");
    }
}

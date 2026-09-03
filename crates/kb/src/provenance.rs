//! Whether an image is downstream of anybody.
//!
//! plan.md makes this a gate on every image rather than an audit at the end,
//! because "our own userland" is a claim and a claim that is never checked is
//! a claim that quietly stops being true.

use crate::build::Engine;
use crate::err::{Result, bail};
use crate::image::{self, Image};
use crate::store;
use crate::target::Target;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

/// Paths that only exist inside the build container. Any of them in a
/// shipped file means the build leaked its scaffolding into the image.
const BUILD_ONLY_PATHS: &[&str] = &[store::C_STORE, store::C_WORK, store::C_SYSROOT];

/// Where the loader looks, before anything a binary asks for itself.
const LIBRARY_PATH: &[&str] = &["/usr/lib"];

pub fn check(engine: &Engine, target: &Target) -> Result<()> {
    let image = Image::of(engine, target);
    if !image.exists() {
        bail!(
            "no image for {}\n  run: kb image {}",
            target.name,
            target.name
        );
    }
    let manifest = image::read_manifest(&image.manifest)?;
    let seed_triple = engine.seed_triple()?;
    let mut problems = Vec::new();

    for id in manifest.values().collect::<std::collections::BTreeSet<_>>() {
        if !engine.store.is_built(id) {
            problems.push(format!("{id}: in the manifest, but no such finished build"));
        }
    }

    let files = image::files_under(&image.rootfs)?;
    for path in &files {
        if !manifest.contains_key(path) {
            problems.push(format!("{path}: in the image, but from no build we did"));
        }
        let full = image.rootfs.join(path);
        let meta = full.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            if resolve(&image.rootfs, &format!("/{path}")).is_none() {
                problems.push(format!(
                    "{path}: a symlink to {}, which this image does not have",
                    fs::read_link(&full)?.display()
                ));
            }
            continue;
        }
        let bytes = fs::read(&full)?;

        for needle in BUILD_ONLY_PATHS {
            if find(&bytes, needle.as_bytes()) {
                problems.push(format!("{path}: contains the build path {needle}"));
            }
        }
        if find(&bytes, seed_triple.as_bytes()) {
            problems.push(format!("{path}: contains the seed's own triple {seed_triple}"));
        }

        if crate::elf::is_elf(&bytes) {
            problems.extend(check_elf(&image.rootfs, path, &bytes));
        } else if bytes.starts_with(b"#!") {
            problems.extend(check_shebang(&image.rootfs, path, &bytes));
        }
    }

    if problems.is_empty() {
        println!(
            "provenance {}: {} files, every one ours",
            target.name,
            files.len()
        );
        return Ok(());
    }
    for p in &problems {
        eprintln!("  {p}");
    }
    bail!("{} provenance problem(s) in the {} image", problems.len(), target.name)
}

fn check_elf(rootfs: &Path, path: &str, bytes: &[u8]) -> Vec<String> {
    let elf = match crate::elf::read(bytes) {
        Ok(elf) => elf,
        Err(e) => return vec![format!("{path}: {e}")],
    };
    let mut problems = Vec::new();

    if let Some(interp) = &elf.interpreter
        && resolve(rootfs, interp).is_none()
    {
        problems.push(format!(
            "{path}: asks for the loader {interp}, which this image does not have"
        ));
    }
    for dir in &elf.run_paths {
        if BUILD_ONLY_PATHS.iter().any(|p| dir.starts_with(p)) {
            problems.push(format!("{path}: searches {dir}, which exists only in the build"));
        }
    }
    // $ORIGIN is the directory the object itself lives in; glibc's gconv
    // modules find each other that way
    let origin = match path.rsplit_once('/') {
        Some((dir, _)) => format!("/{dir}"),
        None => "/".to_string(),
    };
    for lib in &elf.needed {
        let found = LIBRARY_PATH
            .iter()
            .map(|d| d.to_string())
            .chain(elf.run_paths.iter().map(|d| d.replace("$ORIGIN", &origin)))
            .any(|dir| resolve(rootfs, &format!("{dir}/{lib}")).is_some());
        if !found {
            problems.push(format!("{path}: needs {lib}, which this image does not have"));
        }
    }
    problems
}

fn check_shebang(rootfs: &Path, path: &str, bytes: &[u8]) -> Vec<String> {
    let line = bytes
        .iter()
        .position(|b| *b == b'\n')
        .map(|end| &bytes[2..end])
        .unwrap_or(&bytes[2..]);
    let line = String::from_utf8_lossy(line);
    let Some(interpreter) = line.split_whitespace().next() else {
        return vec![format!("{path}: an empty #! line")];
    };
    if resolve(rootfs, interpreter).is_none() {
        return vec![format!("{path}: runs {interpreter}, which this image does not have")];
    }
    Vec::new()
}

/// Resolve an absolute path *inside* the image, following symlinks as the
/// target would. An absolute symlink means the image root, not the host's —
/// getting that wrong is how a check like this passes on the build machine
/// and fails on the machine that boots.
fn resolve(rootfs: &Path, path: &str) -> Option<PathBuf> {
    let mut queue: VecDeque<String> = split(path);
    let mut at = rootfs.to_path_buf();
    let mut hops = 0;

    while let Some(part) = queue.pop_front() {
        if part == ".." {
            if at != rootfs {
                at.pop();
            }
            continue;
        }
        let next = at.join(&part);
        let meta = fs::symlink_metadata(&next).ok()?;
        if meta.file_type().is_symlink() {
            hops += 1;
            if hops > 40 {
                return None;
            }
            let target = fs::read_link(&next).ok()?;
            let target = target.to_string_lossy().into_owned();
            if target.starts_with('/') {
                at = rootfs.to_path_buf();
            }
            for part in split(&target).into_iter().rev() {
                queue.push_front(part);
            }
        } else {
            at = next;
        }
    }
    at.exists().then_some(at)
}

fn split(path: &str) -> VecDeque<String> {
    path.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kb-provenance-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_absolute_symlink_stays_inside_the_image() {
        let root = scratch("absolute");
        fs::create_dir_all(root.join("usr/lib")).unwrap();
        fs::write(root.join("usr/lib/libc.so.6"), b"x").unwrap();
        symlink("usr/lib", root.join("lib64")).unwrap();
        symlink("/usr/lib/libc.so.6", root.join("usr/lib/libc.so")).unwrap();

        assert!(resolve(&root, "/lib64/libc.so.6").is_some(), "through a relative link");
        assert!(resolve(&root, "/usr/lib/libc.so").is_some(), "through an absolute link");
        // The host has one of these; the image does not, and that is the answer.
        assert!(resolve(&root, "/bin/sh").is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_dangling_symlink_does_not_resolve() {
        let root = scratch("dangling");
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        symlink("usr/sbin/dinit", root.join("init")).unwrap();
        assert!(resolve(&root, "/init").is_none());
        fs::write(root.join("usr/bin/dinit"), b"x").unwrap();
        symlink("usr/bin/dinit", root.join("init2")).unwrap();
        assert!(resolve(&root, "/init2").is_some());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_shebang_pointing_outside_the_image_is_a_problem() {
        let root = scratch("shebang");
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        fs::write(root.join("usr/bin/bash"), b"x").unwrap();
        symlink("usr/bin", root.join("bin")).unwrap();

        assert!(check_shebang(&root, "s", b"#!/bin/bash\nexit\n").is_empty());
        let bad = check_shebang(&root, "s", b"#!/usr/bin/python3\n");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("does not have"), "{}", bad[0]);
        fs::remove_dir_all(&root).unwrap();
    }
}

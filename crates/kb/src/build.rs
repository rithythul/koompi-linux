//! Running a build.
//!
//! Every build is a generated bash script run inside the seed container with
//! no network. The script is written to the work directory and kept, so a
//! failed build can be read in full rather than reconstructed: `cat
//! build/work/<id>/build.sh` is the whole truth about what ran.
//!
//! Recipe arguments are emitted inside double quotes, so `$OUT` and `$TRIPLE`
//! expand and spaces are safe. That is the entire substitution mechanism: bash
//! already had one.

use crate::err::{Error, Result, bail};
use crate::recipe::{Kind, Recipe, System};
use crate::store::{self, Store};
use crate::target::Target;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Engine {
    pub root: PathBuf,
    pub build_dir: PathBuf,
    pub store: Store,
    pub seed: String,
    pub jobs: usize,
}

impl Engine {
    pub fn new(root: &Path, jobs: usize) -> Result<Engine> {
        let build_dir = root.join("build");
        let store = Store::new(&build_dir)?;
        let seed_file = root.join("seed/DIGEST");
        let seed = fs::read_to_string(&seed_file)
            .map_err(|e| Error::new(format!("{}: {e}\nbuild the seed first: podman build -f seed/Containerfile seed/", seed_file.display())))?
            .trim()
            .to_string();
        if !seed.starts_with("sha256:") {
            bail!("{}: expected a sha256: digest, found `{seed}`", seed_file.display());
        }
        Ok(Engine {
            root: root.to_path_buf(),
            build_dir,
            store,
            seed,
            jobs,
        })
    }

    /// Build `recipe` and everything under it, skipping what is already built.
    /// Returns the build id of `recipe`.
    pub fn build(
        &self,
        recipes: &BTreeMap<String, Recipe>,
        root_name: &str,
        target: &Target,
    ) -> Result<String> {
        let order = crate::graph::order(recipes, root_name)?;
        let mut ids: BTreeMap<String, String> = BTreeMap::new();

        for name in &order {
            let recipe = &recipes[name];
            let id = id_of(recipe, target, &ids);
            if !self.store.is_built(&id) {
                self.build_one(recipes, recipe, target, &id, &ids)?;
            } else {
                println!("  ok  {name} {} (cached)", recipe.version);
            }
            ids.insert(name.clone(), id);
        }
        Ok(ids[root_name].clone())
    }

    /// The sysroot a recipe would be built against, for looking at rather
    /// than building with. Everything it needs must already be built: this
    /// answers "where is it", not "make it".
    pub fn sysroot_of(
        &self,
        recipes: &BTreeMap<String, Recipe>,
        root_name: &str,
        target: &Target,
    ) -> Result<PathBuf> {
        let order = crate::graph::order(recipes, root_name)?;
        let mut ids: BTreeMap<String, String> = BTreeMap::new();
        for name in &order {
            let id = id_of(&recipes[name], target, &ids);
            if !self.store.is_built(&id) {
                bail!(
                    "{name} is not built for {t}\n  run: kb build {root_name} --target {t}",
                    t = target.name
                );
            }
            ids.insert(name.clone(), id);
        }
        let dep_ids: Vec<(&str, Kind, String)> = order
            .iter()
            .filter(|n| *n != root_name)
            .map(|n| (n.as_str(), recipes[n].kind, ids[n].clone()))
            .collect();
        store::sysroot(&self.build_dir, &self.store, &dep_ids)
    }

    fn build_one(
        &self,
        recipes: &BTreeMap<String, Recipe>,
        recipe: &Recipe,
        target: &Target,
        id: &str,
        ids: &BTreeMap<String, String>,
    ) -> Result<()> {
        // Everything this recipe transitively needs, with its kind, so
        // host tools go on PATH and target packages go in the sysroot.
        let closure = crate::graph::order(recipes, &recipe.name)?;
        // Every name in the closure was ordered before this one, so its id is
        // already known; that is the invariant graph::order exists to provide.
        let dep_ids: Vec<(&str, Kind, String)> = closure
            .iter()
            .filter(|n| *n != &recipe.name)
            .map(|n| (n.as_str(), recipes[n].kind, ids[n].clone()))
            .collect();

        let sysroot = store::sysroot(&self.build_dir, &self.store, &dep_ids)?;
        let tarball = self.fetch(recipe)?;

        let work = self.build_dir.join("work").join(id);
        store::remove_tree(&work)?;
        fs::create_dir_all(&work)?;
        let out = self.store.prepare(id)?;

        let script = script(self.jobs, recipe, target, id, &dep_ids, &tarball);
        let script_path = work.join("build.sh");
        fs::write(&script_path, &script)?;

        println!("build {} {} for {}", recipe.name, recipe.version, target.name);
        let started = Instant::now();
        let status = Command::new("podman")
            .args(["run", "--rm", "--network=none"])
            .arg("-v")
            .arg(format!("{}:{}:ro", self.store.root.display(), store::C_STORE))
            .arg("-v")
            .arg(format!("{}:{}/{id}", out.display(), store::C_STORE))
            .arg("-v")
            .arg(format!("{}:{}", work.display(), store::C_WORK))
            .arg("-v")
            .arg(format!("{}:{}:ro", self.root.join("sources").display(), store::C_SOURCES))
            .arg("-v")
            .arg(format!("{}:{}:ro", sysroot.display(), store::C_SYSROOT))
            .arg("-w")
            .arg(store::C_WORK)
            .arg(&self.seed)
            .args(["bash", "-euo", "pipefail"])
            .arg(format!("{}/build.sh", store::C_WORK))
            .status()
            .map_err(|e| Error::new(format!("running podman: {e}")))?;
        let secs = started.elapsed().as_secs();

        self.record(recipe, target, secs, status.success());

        if !status.success() {
            self.store.discard(id);
            let log = work.join("build.log");
            if let Ok(text) = fs::read_to_string(&log) {
                let tail: Vec<&str> = text.lines().rev().take(40).collect();
                for line in tail.into_iter().rev() {
                    eprintln!("  | {line}");
                }
            }
            bail!(
                "{} {} failed after {secs}s\n  script: {}\n  log:    {}",
                recipe.name,
                recipe.version,
                script_path.display(),
                log.display()
            );
        }

        // A build that exits 0 and installs nothing is the worst kind of
        // success: linux-headers did exactly this, because INSTALL_HDR_PATH
        // was exported as an environment variable and the kernel's makefile
        // assigns it with `=`, which overrides the environment. It took 13
        // seconds and reported ok.
        if !contains_a_file(&out)? {
            self.store.discard(id);
            bail!(
                "{} {} installed nothing into $OUT\n  \
                 a make variable set in [build.env] can be overridden by the makefile; \
                 pass it in `make`/`install` instead, where the command line always wins\n  \
                 script: {}",
                recipe.name,
                recipe.version,
                script_path.display()
            );
        }

        self.store.finalize(
            id,
            &format!(
                "name={}\nversion={}\ntarget={}\nseconds={secs}\n",
                recipe.name, recipe.version, target.name
            ),
        )?;
        store::remove_tree(&work)?;
        println!("  ok  {} {} in {secs}s", recipe.name, recipe.version);
        Ok(())
    }

    /// Fetch on the host, because the build container has no network.
    /// Verified before it is put where a build can see it.
    fn fetch(&self, recipe: &Recipe) -> Result<String> {
        let dir = self.root.join("sources");
        fs::create_dir_all(&dir)?;
        let name = recipe.tarball().to_string();
        let final_path = dir.join(&name);

        if final_path.exists() {
            let got = crate::sha256::digest_file(&final_path)?;
            if got == recipe.source.sha256 {
                return Ok(name);
            }
            bail!(
                "{} has sha256 {got}, but {} pins {}\nremove it if upstream was re-rolled, but check why first",
                final_path.display(),
                recipe.path.display(),
                recipe.source.sha256
            );
        }

        println!("fetch {}", recipe.source.url);
        let part = dir.join(format!("{name}.part"));
        let status = Command::new("curl")
            .args(["-fsSL", "--retry", "3", "-o"])
            .arg(&part)
            .arg(&recipe.source.url)
            .status()
            .map_err(|e| Error::new(format!("running curl: {e}")))?;
        if !status.success() {
            let _ = fs::remove_file(&part);
            bail!("could not fetch {}", recipe.source.url);
        }

        let got = crate::sha256::digest_file(&part)?;
        if got != recipe.source.sha256 {
            let _ = fs::remove_file(&part);
            bail!(
                "{} has sha256 {got}, but {} pins {}",
                recipe.source.url,
                recipe.path.display(),
                recipe.source.sha256
            );
        }
        fs::rename(&part, &final_path)?;
        Ok(name)
    }

}

/// The whole build, as bash. Kept as a free function because it is pure text
/// and the only way to be sure of a generated script is to read one.
pub fn script(
    jobs: usize,
    recipe: &Recipe,
    target: &Target,
    id: &str,
    dep_ids: &[(&str, Kind, String)],
    tarball: &str,
) -> String {
    {
        let mut s = String::new();
        s.push_str(&format!(
            "# generated by kb -- do not edit, it is rewritten every build\n\
             # recipe: {} {}   target: {}   id: {id}\n\n",
            recipe.name, recipe.version, target.name
        ));
        // Everything after this goes to the log, so podman's own output stays
        // empty unless the container itself failed to start.
        s.push_str("exec > /kb/work/build.log 2>&1\nset -x\n\n");

        let path_extra: Vec<String> = dep_ids
            .iter()
            .filter(|(_, kind, _)| *kind == Kind::HostTool)
            .map(|(_, _, id)| format!("{}/{id}/bin", store::C_STORE))
            .collect();

        s.push_str(&format!("export OUT={}/{id}\n", store::C_STORE));
        s.push_str(&format!("export SYSROOT={}\n", store::C_SYSROOT));
        s.push_str(&format!("export TRIPLE=\"{}\"\n", target.triple));
        s.push_str(&format!("export ARCH=\"{}\"\n", target.arch));
        s.push_str(&format!("export KARCH=\"{}\"\n", target.kernel_arch));
        s.push_str(&format!("export JOBS={jobs}\n"));
        s.push_str("export BUILD_TRIPLE=\"$(gcc -dumpmachine)\"\n");
        if !path_extra.is_empty() {
            s.push_str(&format!("export PATH=\"{}:$PATH\"\n", path_extra.join(":")));
        }

        // The seed's own gcc, g++ and ar are on PATH, because the seed is
        // where make, perl and sed come from. Nothing stops a target build
        // from picking them up: glibc's configure found the host g++, decided
        // C++ was available, and then failed at link time with `cannot find
        // -lstdc++` because the *cross* linker was doing the linking.
        //
        // So a target build is told which toolchain is its own. A recipe can
        // still override any of these, because recipe env is emitted after.
        //
        // LD is deliberately absent: builds are expected to link through the
        // compiler driver, and setting LD confuses libtool more than it helps.
        if recipe.kind == Kind::Target {
            for tool in ["CC=gcc", "CXX=g++", "AR=ar", "RANLIB=ranlib", "NM=nm",
                         "STRIP=strip", "OBJCOPY=objcopy", "OBJDUMP=objdump",
                         "READELF=readelf"] {
                let (name, bin) = tool.split_once('=').expect("literal has an =");
                s.push_str(&format!("export {name}=\"$TRIPLE-{bin}\"\n"));
            }
        }
        for (k, v) in &recipe.build.env {
            s.push_str(&format!("export {k}=\"{v}\"\n"));
        }

        s.push_str(&format!(
            // --no-same-owner: tar runs as root in the container and would
            // otherwise restore the uids recorded in the tarball. gcc's ships
            // uid 1000, which maps into the host's subuid range, and the
            // result is a scratch tree the invoking user cannot delete. That
            // is the post-mortem's uid-100997 trap wearing a different hat.
            "\nmkdir -p {work}/src\ntar -xf {src}/{tarball} -C {work}/src --no-same-owner --strip-components={strip}\nexport SRC={work}/src\n\n",
            work = store::C_WORK,
            src = store::C_SOURCES,
            strip = recipe.source.strip,
        ));

        match recipe.build.system {
            System::Shell => {
                s.push_str(recipe.build.script.as_deref().unwrap_or(""));
                s.push('\n');
            }
            System::Configure => {
                if recipe.build.out_of_tree {
                    s.push_str(&format!("mkdir -p {}/bld\ncd {}/bld\n", store::C_WORK, store::C_WORK));
                    s.push_str("\"$SRC/configure\"");
                } else {
                    s.push_str("cd \"$SRC\"\n./configure");
                }
                for a in &recipe.build.configure {
                    s.push_str(&format!(" \\\n  \"{a}\""));
                }
                s.push_str("\n\n");
                s.push_str(&make_step("make -j\"$JOBS\"", &recipe.build.make, &["all".into()]));
                s.push_str(&make_step("make", &recipe.build.install, &["install".into()]));
            }
            System::Make => {
                s.push_str("cd \"$SRC\"\n\n");
                s.push_str(&make_step("make -j\"$JOBS\"", &recipe.build.make, &["all".into()]));
                s.push_str(&make_step("make", &recipe.build.install, &["install".into()]));
            }
        }
        s
    }
}

impl Engine {
    /// One line per build, which is how throughput gets measured rather than
    /// remembered. Gate 0 criterion 4 needs a number, not an impression.
    fn record(&self, recipe: &Recipe, target: &Target, secs: u64, ok: bool) {
        use std::io::Write;
        let path = self.build_dir.join("builds.tsv");
        let fresh = !path.exists();
        let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
            return;
        };
        if fresh {
            let _ = writeln!(f, "epoch\tname\tversion\ttarget\tseconds\tresult");
        }
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            f,
            "{epoch}\t{}\t{}\t{}\t{secs}\t{}",
            recipe.name,
            recipe.version,
            target.name,
            if ok { "ok" } else { "fail" }
        );
    }
}

/// Is there anything at all under `dir`? Empty directories do not count:
/// `make install` can create a tree and populate none of it.
fn contains_a_file(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // symlink_metadata: a dangling symlink is still something installed.
        if !entry.file_type()?.is_dir() {
            return Ok(true);
        }
        if contains_a_file(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `None` means the recipe said nothing, so run the usual thing.
/// `Some([])` means the recipe said "not this step", so run nothing.
/// A recipe's build id, given the ids of everything it depends on.
/// Both the builder and `sysroot_of` go through here, so they cannot drift.
fn id_of(recipe: &Recipe, target: &Target, ids: &BTreeMap<String, String>) -> String {
    let deps: BTreeMap<String, String> = recipe
        .all_deps()
        .map(|d| (d.to_string(), ids[d].clone()))
        .collect();
    store::build_id(recipe, target, &deps)
}

fn make_step(prefix: &str, args: &Option<Vec<String>>, fallback: &[String]) -> String {
    let args = match args {
        None => fallback,
        Some(a) if a.is_empty() => return String::new(),
        Some(a) => a.as_slice(),
    };
    let mut s = prefix.to_string();
    for a in args {
        s.push_str(&format!(" \"{a}\""));
    }
    s.push_str("\n\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{Build, Deps, Source, System};
    use crate::target::Target;

    fn target() -> Target {
        Target {
            name: "cloud".into(),
            triple: "x86_64-koompi-linux-gnu".into(),
            arch: "x86_64".into(),
            kernel_arch: "x86".into(),
        }
    }

    fn recipe(system: System, make: Option<Vec<&str>>, install: Option<Vec<&str>>) -> Recipe {
        Recipe {
            name: "pkg".into(),
            version: "1.0".into(),
            kind: Kind::Target,
            source: Source {
                url: "https://example.invalid/pkg-1.0.tar.xz".into(),
                sha256: "0".repeat(64),
                strip: 1,
            },
            build: Build {
                system,
                out_of_tree: true,
                configure: vec!["--prefix=$OUT".into(), "--host=$TRIPLE".into()],
                make: make.map(|v| v.iter().map(|s| s.to_string()).collect()),
                install: install.map(|v| v.iter().map(|s| s.to_string()).collect()),
                env: vec![("ARCH".into(), "$KARCH".into())],
                script: None,
            },
            deps: Deps { build: vec![], runtime: vec![] },
            path: "recipes/pkg.toml".into(),
            raw: Vec::new(),
        }
    }

    fn render(r: &Recipe, deps: &[(&str, Kind, String)]) -> String {
        script(4, r, &target(), "abc-pkg-1.0", deps, "pkg-1.0.tar.xz")
    }

    #[test]
    fn exports_the_target_and_unpacks_the_source() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.contains("export TRIPLE=\"x86_64-koompi-linux-gnu\""), "{s}");
        assert!(s.contains("export KARCH=\"x86\""), "{s}");
        assert!(s.contains("export OUT=/kb/store/abc-pkg-1.0"), "{s}");
        assert!(s.contains("export JOBS=4"), "{s}");
        // Recipe env comes after the engine's, so it can refer to it.
        assert!(s.find("export KARCH").unwrap() < s.find("export ARCH=\"$KARCH\"").unwrap());
        assert!(s.contains("tar -xf /kb/sources/pkg-1.0.tar.xz -C /kb/work/src --no-same-owner --strip-components=1"), "{s}");
    }

    #[test]
    fn arguments_are_quoted_so_variables_expand() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.contains("\"$SRC/configure\""), "{s}");
        assert!(s.contains("\"--prefix=$OUT\""), "{s}");
        assert!(s.contains("\"--host=$TRIPLE\""), "{s}");
    }

    #[test]
    fn absent_means_the_usual_step_and_empty_means_no_step() {
        let usual = render(&recipe(System::Configure, None, None), &[]);
        assert!(usual.contains("make -j\"$JOBS\" \"all\""), "{usual}");
        assert!(usual.contains("make \"install\""), "{usual}");

        // linux-headers: nothing to build, everything to install.
        let headers = render(
            &recipe(System::Make, Some(vec![]), Some(vec!["headers_install"])),
            &[],
        );
        assert!(!headers.contains("make -j"), "a build step was generated:\n{headers}");
        assert!(headers.contains("make \"headers_install\""), "{headers}");
    }

    #[test]
    fn host_tool_deps_go_on_path_and_target_deps_do_not() {
        let deps = [
            ("binutils", Kind::HostTool, "aaa-binutils-2.47".to_string()),
            ("glibc", Kind::Target, "bbb-glibc-2.44".to_string()),
        ];
        let s = render(&recipe(System::Configure, None, None), &deps);
        assert!(s.contains("export PATH=\"/kb/store/aaa-binutils-2.47/bin:$PATH\""), "{s}");
        assert!(!s.contains("bbb-glibc-2.44"), "a sysroot package reached PATH:\n{s}");
    }

    #[test]
    fn a_target_build_is_pointed_at_the_cross_toolchain() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.contains("export CC=\"$TRIPLE-gcc\""), "{s}");
        assert!(s.contains("export CXX=\"$TRIPLE-g++\""), "{s}");
        assert!(s.contains("export RANLIB=\"$TRIPLE-ranlib\""), "{s}");
        // Linking goes through the compiler driver; setting LD confuses libtool.
        assert!(!s.contains("export LD="), "{s}");
        // The recipe's own env comes after, so it can override any of them.
        // Matched on the recipe's line specifically: the engine exports an
        // ARCH of its own earlier, and find() would have returned that one.
        let recipe_env = s.find(r#"export ARCH="$KARCH""#).expect("recipe env");
        assert!(s.find("export CC=").unwrap() < recipe_env, "{s}");
    }

    #[test]
    fn a_host_tool_build_uses_the_seed_compiler() {
        let mut r = recipe(System::Configure, None, None);
        r.kind = Kind::HostTool;
        let s = render(&r, &[]);
        assert!(!s.contains("export CC="), "a host tool was cross-configured:\n{s}");
    }

    #[test]
    fn output_goes_to_the_log_so_a_failure_can_be_read_afterwards() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.starts_with("# generated by kb"), "{s}");
        assert!(s.contains("exec > /kb/work/build.log 2>&1"), "{s}");
        assert!(s.contains("set -x"), "{s}");
    }
}

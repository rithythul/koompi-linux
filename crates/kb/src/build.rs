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
use crate::recipe::{Kind, Recipe, Source, System};
use crate::store::{self, Store};
use crate::target::Target;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// DESIGN.md C10: one project-wide epoch, so no recipe carries a date.
/// 2026-09-01T00:00:00Z, the day spec.md was settled.
pub const SOURCE_DATE_EPOCH: u64 = 1788220800;

/// The target's kernel config fragment, mounted read-only into every build.
pub const C_KERNEL_CONFIG: &str = "/kb/kernel.config";
const KERNEL_CONFIG_VAR_NAME: &str = "KERNEL_CONFIG";
/// How a recipe spells it, and how the engine knows the recipe read it.
pub const KERNEL_CONFIG_VAR: &str = "$KERNEL_CONFIG";

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
        let ids = self.build_all(recipes, std::slice::from_ref(&root_name.to_string()), target)?;
        Ok(ids[root_name].clone())
    }

    /// Build every root and everything under them. Returns the id of every
    /// recipe touched, which is what an image needs to know where things are.
    pub fn build_all(
        &self,
        recipes: &BTreeMap<String, Recipe>,
        roots: &[String],
        target: &Target,
    ) -> Result<BTreeMap<String, String>> {
        let mut ids: BTreeMap<String, String> = BTreeMap::new();
        for root in roots {
            for name in &crate::graph::order(recipes, root)? {
                if ids.contains_key(name) {
                    continue;
                }
                let recipe = &recipes[name];
                let id = id_of(recipe, target, &ids);
                if !self.store.is_built(&id) {
                    self.build_one(recipes, recipe, target, &id, &ids)?;
                } else {
                    println!("  ok  {name} {} (cached)", recipe.version);
                }
                ids.insert(name.clone(), id);
            }
        }
        Ok(ids)
    }

    /// What the seed's own compiler calls the machine it runs on. Anything
    /// carrying that string into an image was built for the seed, not for us.
    pub fn seed_triple(&self) -> Result<String> {
        let out = Command::new("podman")
            .args(["run", "--rm", "--network=none", &self.seed, "gcc", "-dumpmachine"])
            .output()
            .map_err(|e| Error::new(format!("running podman: {e}")))?;
        let triple = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || triple.is_empty() {
            bail!("the seed's gcc did not say what machine it targets");
        }
        Ok(triple)
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
        let tarball = match &recipe.source {
            Some(source) => Some(self.fetch(recipe, source)?),
            None => None,
        };

        let work = self.build_dir.join("work").join(id);
        store::remove_tree(&work)?;
        fs::create_dir_all(&work)?;
        let out = self.store.prepare(id)?;

        let script = script(self.jobs, recipe, target, id, &dep_ids, tarball.as_deref());
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
            .arg("-v")
            .arg(format!(
                "{}:{}:ro",
                self.root.join(&target.kernel_config).display(),
                C_KERNEL_CONFIG
            ))
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

        // a host tool installs at its store path, so its bin/ is its prefix
        if recipe.kind == Kind::Target
            && let Some(dir) = installs_beside_usr(&out)?
        {
            self.store.discard(id);
            bail!(
                "{} {} installed a /{dir} directory\n  \
                 /bin, /sbin, /lib and /lib64 are symlinks into /usr in this core; \
                 tell the build to install under /usr (--bindir, --sbindir, --libdir or their equivalent)\n  \
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
    fn fetch(&self, recipe: &Recipe, source: &Source) -> Result<String> {
        let dir = self.root.join("sources");
        fs::create_dir_all(&dir)?;
        let name = source.tarball().to_string();
        let final_path = dir.join(&name);

        if final_path.exists() {
            let got = crate::sha256::digest_file(&final_path)?;
            if got == source.sha256 {
                return Ok(name);
            }
            bail!(
                "{} has sha256 {got}, but {} pins {}\nremove it if upstream was re-rolled, but check why first",
                final_path.display(),
                recipe.path.display(),
                source.sha256
            );
        }

        println!("fetch {}", source.url);
        let part = dir.join(format!("{name}.part"));
        let status = Command::new("curl")
            .args(["-fsSL", "--retry", "3", "-o"])
            .arg(&part)
            .arg(&source.url)
            .status()
            .map_err(|e| Error::new(format!("running curl: {e}")))?;
        if !status.success() {
            let _ = fs::remove_file(&part);
            bail!("could not fetch {}", source.url);
        }

        let got = crate::sha256::digest_file(&part)?;
        if got != source.sha256 {
            let _ = fs::remove_file(&part);
            bail!(
                "{} has sha256 {got}, but {} pins {}",
                source.url,
                recipe.path.display(),
                source.sha256
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
    tarball: Option<&str>,
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

        // What a recipe declares comes before what that drags in: gcc and
        // gcc-bootstrap both install `$TRIPLE-gcc`, and the bootstrap one,
        // built without headers, thinks MB_LEN_MAX is 1.
        let direct: Vec<&str> = recipe.all_deps().collect();
        let (declared, dragged_in): (Vec<_>, Vec<_>) = dep_ids
            .iter()
            .filter(|(_, kind, _)| *kind == Kind::HostTool)
            .partition(|(n, _, _)| direct.contains(n));
        let path_extra: Vec<String> = declared
            .iter()
            .chain(dragged_in.iter())
            .map(|(_, _, id)| format!("{}/{id}/bin", store::C_STORE))
            .collect();

        s.push_str(&format!("export OUT={}/{id}\n", store::C_STORE));
        s.push_str(&format!("export SYSROOT={}\n", store::C_SYSROOT));
        s.push_str(&format!("export TRIPLE=\"{}\"\n", target.triple));
        s.push_str(&format!("export ARCH=\"{}\"\n", target.arch));
        s.push_str(&format!("export KARCH=\"{}\"\n", target.kernel_arch));
        s.push_str(&format!("export JOBS={jobs}\n"));
        // an empty $SRC for a recipe with no upstream, so a script that cds
        // into it fails the same way whatever the recipe looks like
        s.push_str(&format!("export SRC={}/src\n", store::C_WORK));
        s.push_str(&format!("export {KERNEL_CONFIG_VAR_NAME}={C_KERNEL_CONFIG}\n"));
        s.push_str("export BUILD_TRIPLE=\"$(gcc -dumpmachine)\"\n");
        // DESIGN.md C10: what a deterministic build needs from its environment
        s.push_str(&format!("export SOURCE_DATE_EPOCH={SOURCE_DATE_EPOCH}\n"));
        s.push_str("export TZ=UTC\nexport LC_ALL=C.UTF-8\numask 022\n");
        if !path_extra.is_empty() {
            s.push_str(&format!("export PATH=\"{}:$PATH\"\n", path_extra.join(":")));
        }

        // Where each direct dependency landed, so a recipe can point at one
        // by name instead of guessing. gcc needs this: it has to be told
        // where its assembler is, and the answer is a store path.
        //
        // Direct dependencies only. A recipe that reaches for something it
        // did not declare has a dependency it is not admitting to.
        for (name, _, id) in dep_ids.iter().filter(|(n, _, _)| direct.contains(n)) {
            let var = name.to_uppercase().replace('-', "_");
            s.push_str(&format!("export KB_{var}={}/{id}\n", store::C_STORE));
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
        // C10: no build path reaches a binary; C5: the target's hardening
        // policy. Every build sees them by name, because a host tool can
        // still emit target code (gcc builds libstdc++); only a target build
        // gets them as its CFLAGS. A recipe that cannot take a flag strips it.
        let cflags = format!(
            "-ffile-prefix-map=$SRC=/src -ffile-prefix-map={}=/work {}",
            store::C_WORK,
            target.cflags.join(" ")
        );
        s.push_str(&format!("export TARGET_CFLAGS=\"{}\"\n", cflags.trim_end()));
        s.push_str(&format!("export TARGET_LDFLAGS=\"{}\"\n", target.ldflags.join(" ")));
        if recipe.kind == Kind::Target {
            for tool in ["CC=gcc", "CXX=g++", "AR=ar", "RANLIB=ranlib", "NM=nm",
                         "STRIP=strip", "OBJCOPY=objcopy", "OBJDUMP=objdump",
                         "READELF=readelf"] {
                let (name, bin) = tool.split_once('=').expect("literal has an =");
                s.push_str(&format!("export {name}=\"$TRIPLE-{bin}\"\n"));
            }
            s.push_str("export CFLAGS=\"$TARGET_CFLAGS\"\n");
            s.push_str("export CXXFLAGS=\"$TARGET_CFLAGS\"\n");
            s.push_str("export LDFLAGS=\"$TARGET_LDFLAGS\"\n");
        }
        for (k, v) in &recipe.build.env {
            s.push_str(&format!("export {k}=\"{v}\"\n"));
        }

        s.push_str(&format!("\nmkdir -p {}/src\n", store::C_WORK));
        if let (Some(tarball), Some(source)) = (tarball, &recipe.source) {
            s.push_str(&format!(
                // --no-same-owner: tar runs as root in the container and would
                // otherwise restore the uids recorded in the tarball. gcc's ships
                // uid 1000, which maps into the host's subuid range, and the
                // result is a scratch tree the invoking user cannot delete. That
                // is the post-mortem's uid-100997 trap wearing a different hat.
                "tar -xf {src}/{tarball} -C $SRC --no-same-owner --strip-components={strip}\n",
                src = store::C_SOURCES,
                strip = source.strip,
            ));
        }
        s.push('\n');

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
        for p in &recipe.build.remove {
            s.push_str(&format!("rm \"$OUT/{p}\"\n"));
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

/// The name of a top-level directory that only the layout may provide, if
/// the package installed one. A symlink is fine: that is the layout itself.
fn installs_beside_usr(out: &Path) -> Result<Option<&'static str>> {
    for dir in ["bin", "sbin", "lib", "lib64"] {
        let path = out.join(dir);
        if let Ok(meta) = path.symlink_metadata()
            && meta.is_dir()
        {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

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
        Target::for_test("cloud", "x86_64-koompi-linux-gnu", "x86_64", "x86")
    }

    fn recipe(system: System, make: Option<Vec<&str>>, install: Option<Vec<&str>>) -> Recipe {
        Recipe {
            name: "pkg".into(),
            version: "1.0".into(),
            kind: Kind::Target,
            source: Some(Source {
                url: "https://example.invalid/pkg-1.0.tar.xz".into(),
                sha256: "0".repeat(64),
                license: "MIT".into(),
                strip: 1,
            }),
            build: Build {
                system,
                out_of_tree: true,
                configure: vec!["--prefix=$OUT".into(), "--host=$TRIPLE".into()],
                make: make.map(|v| v.iter().map(|s| s.to_string()).collect()),
                install: install.map(|v| v.iter().map(|s| s.to_string()).collect()),
                env: vec![("ARCH".into(), "$KARCH".into())],
                script: None,
                remove: vec![],
            },
            deps: Deps { build: vec![], runtime: vec![] },
            checks: vec![],
            path: "recipes/pkg.toml".into(),
            raw: Vec::new(),
        }
    }

    fn render(r: &Recipe, deps: &[(&str, Kind, String)]) -> String {
        script(4, r, &target(), "abc-pkg-1.0", deps, Some("pkg-1.0.tar.xz"))
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
        assert!(s.contains("tar -xf /kb/sources/pkg-1.0.tar.xz -C $SRC --no-same-owner --strip-components=1"), "{s}");
    }

    #[test]
    fn a_recipe_with_no_source_still_gets_an_empty_src() {
        let mut r = recipe(System::Configure, None, None);
        r.source = None;
        let s = script(4, &r, &target(), "abc-pkg-1.0", &[], None);
        assert!(!s.contains("tar -xf"), "something was unpacked:\n{s}");
        assert!(s.contains("export SRC=/kb/work/src"), "{s}");
    }

    #[test]
    fn the_kernel_config_fragment_is_where_the_recipe_expects_it() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.contains("export KERNEL_CONFIG=/kb/kernel.config"), "{s}");
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
    fn a_declared_dependency_shadows_what_it_drags_in() {
        let mut r = recipe(System::Configure, None, None);
        r.deps.build = vec!["gcc".into()];
        // closure order: gcc-bootstrap is built before gcc
        let deps = [
            ("binutils", Kind::HostTool, "aaa-binutils-2.47".to_string()),
            ("gcc-bootstrap", Kind::HostTool, "bbb-gcc-bootstrap-15.3.0".to_string()),
            ("gcc", Kind::HostTool, "ccc-gcc-15.3.0".to_string()),
        ];
        let s = render(&r, &deps);
        assert!(
            s.contains("export PATH=\"/kb/store/ccc-gcc-15.3.0/bin:/kb/store/aaa-binutils-2.47/bin:/kb/store/bbb-gcc-bootstrap-15.3.0/bin:$PATH\""),
            "{s}"
        );
    }

    #[test]
    fn direct_dependencies_get_a_path_variable() {
        let mut r = recipe(System::Configure, None, None);
        r.deps.build = vec!["binutils".into(), "gcc-bootstrap".into()];
        let deps = [
            ("binutils", Kind::HostTool, "aaa-binutils-2.47".to_string()),
            ("gcc-bootstrap", Kind::HostTool, "bbb-gcc-bootstrap-15.3.0".to_string()),
            // In the closure but not declared by this recipe.
            ("glibc", Kind::Target, "ccc-glibc-2.44".to_string()),
        ];
        let s = render(&r, &deps);
        assert!(s.contains("export KB_BINUTILS=/kb/store/aaa-binutils-2.47"), "{s}");
        assert!(s.contains("export KB_GCC_BOOTSTRAP=/kb/store/bbb-gcc-bootstrap-15.3.0"), "{s}");
        assert!(!s.contains("KB_GLIBC="), "an undeclared dependency was offered:\n{s}");
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
        assert!(!s.contains("export CFLAGS="), "target policy reached a host tool:\n{s}");
    }

    #[test]
    fn a_target_build_gets_the_targets_hardening_and_no_build_path() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(
            s.contains("export TARGET_CFLAGS=\"-ffile-prefix-map=$SRC=/src -ffile-prefix-map=/kb/work=/work -D_FORTIFY_SOURCE=3\""),
            "{s}"
        );
        assert!(s.contains("export TARGET_LDFLAGS=\"-Wl,-z,relro,-z,now\""), "{s}");
        assert!(s.contains("export CFLAGS=\"$TARGET_CFLAGS\""), "{s}");
        assert!(s.contains("export LDFLAGS=\"$TARGET_LDFLAGS\""), "{s}");
        // $SRC must already be set when the flags mention it
        assert!(s.find("export SRC=").unwrap() < s.find("export TARGET_CFLAGS=").unwrap(), "{s}");
    }

    #[test]
    fn a_host_tool_sees_the_targets_flags_by_name_only() {
        let mut r = recipe(System::Configure, None, None);
        r.kind = Kind::HostTool;
        let s = render(&r, &[]);
        assert!(s.contains("export TARGET_CFLAGS=\""), "{s}");
        assert!(!s.contains("export CFLAGS="), "a host tool was handed target CFLAGS:\n{s}");
    }

    #[test]
    fn what_a_recipe_removes_goes_after_install_and_must_exist() {
        let mut r = recipe(System::Configure, None, None);
        r.build.remove = vec!["usr/bin/bashbug".into()];
        let s = render(&r, &[]);
        assert!(s.contains("rm \"$OUT/usr/bin/bashbug\"\n"), "{s}");
        assert!(s.find("make \"install\"").unwrap() < s.find("rm \"$OUT/usr/bin/bashbug\"").unwrap(), "{s}");
    }

    #[test]
    fn every_build_is_pinned_to_one_epoch_zone_locale_and_umask() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        for line in ["export SOURCE_DATE_EPOCH=1788220800", "export TZ=UTC", "export LC_ALL=C.UTF-8", "umask 022"] {
            assert!(s.contains(line), "missing {line}:\n{s}");
        }
    }

    #[test]
    fn a_real_bin_directory_is_caught_and_the_layout_symlink_is_not() {
        let dir = std::env::temp_dir().join(format!("kb-beside-usr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        std::os::unix::fs::symlink("usr/bin", dir.join("bin")).unwrap();
        assert_eq!(installs_beside_usr(&dir).unwrap(), None);
        fs::create_dir_all(dir.join("sbin")).unwrap();
        assert_eq!(installs_beside_usr(&dir).unwrap(), Some("sbin"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_goes_to_the_log_so_a_failure_can_be_read_afterwards() {
        let s = render(&recipe(System::Configure, None, None), &[]);
        assert!(s.starts_with("# generated by kb"), "{s}");
        assert!(s.contains("exec > /kb/work/build.log 2>&1"), "{s}");
        assert!(s.contains("set -x"), "{s}");
    }
}

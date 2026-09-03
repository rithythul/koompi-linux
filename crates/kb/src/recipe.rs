//! What a recipe is.
//!
//! A recipe never names an architecture. Everything that varies per target
//! reaches the build as an environment variable, so `configure` arguments are
//! written as `"--host=$TRIPLE"` and bash does the expansion. That is why
//! there is no substitution engine here: there is nothing for it to do.
//!
//! `kb lint` enforces the no-architecture rule; see `lint.rs`.

use crate::err::{Result, bail};
use crate::read::Reader;
use crate::toml;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Runs on the build machine and never enters an image: a cross compiler.
    /// Its output is put on `PATH` for anything that depends on it.
    HostTool,
    /// Runs on the target. Its output is merged into the sysroot for anything
    /// that depends on it, and it is a candidate for an image.
    Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    Configure,
    Make,
    /// The escape hatch from spec.md decision 2. Counted in the Gate 0 report:
    /// if most recipes need it, the format is wrong.
    Shell,
}

#[derive(Debug)]
pub struct Source {
    pub url: String,
    pub sha256: String,
    /// SPDX. What an image's bill of materials is made of, and the one
    /// field that is trivial at 13 recipes and painful at 150.
    pub license: String,
    /// Leading path components to strip when unpacking.
    pub strip: u32,
}

impl Source {
    /// The basename the tarball is stored under.
    pub fn tarball(&self) -> &str {
        self.url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("source")
    }
}

#[derive(Debug)]
pub struct Build {
    pub system: System,
    /// Configure out of tree. Ignored unless `system = "configure"`.
    pub out_of_tree: bool,
    pub configure: Vec<String>,
    /// Arguments to `make` for the build step. `None` means the default
    /// target; `Some([])` means there is no build step at all, which is not
    /// the same thing -- `linux-headers` installs without building.
    pub make: Option<Vec<String>>,
    /// Arguments to `make` for the install step. `None` means plain
    /// `make install`; `Some([])` means the build step already installed.
    pub install: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    /// Only for `system = "shell"`.
    pub script: Option<String>,
    /// Paths under $OUT the install step creates and the image must not
    /// carry: a bug-report script with CFLAGS baked in, a Makefile for
    /// building against the package. Removed with a plain `rm` after
    /// install, so a stale entry fails the build instead of lingering.
    pub remove: Vec<String>,
}

#[derive(Debug)]
pub struct Deps {
    pub build: Vec<String>,
    /// Kept separate from `build` on purpose. Attempt one's planner broke a
    /// dependency cycle by cutting a runtime edge, assuming the chroot already
    /// had it; over a minimal seed it did not, and the failure surfaced far
    /// from its cause. Cutting a runtime edge is a hard error here.
    pub runtime: Vec<String>,
}

#[derive(Debug)]
pub struct Recipe {
    pub name: String,
    pub version: String,
    pub kind: Kind,
    /// Absent for a recipe that has no upstream: the filesystem layout and
    /// the toolchain's own runtime libraries are ours, and inventing a
    /// tarball to hold them would be a lie the format then has to carry.
    pub source: Option<Source>,
    pub build: Build,
    pub deps: Deps,
    /// DESIGN.md C9: commands the image's selftest runs for this recipe,
    /// under QEMU, on every target. Green means they ran, not that it built.
    pub checks: Vec<String>,
    pub path: PathBuf,
    /// The file as it was on disk, hashed into the build id.
    pub raw: Vec<u8>,
}

impl Recipe {
    pub fn load(path: &Path) -> Result<Recipe> {
        let raw = fs::read(path).map_err(|e| {
            crate::err::Error::new(format!("{}: {e}", path.display()))
        })?;
        let text = String::from_utf8(raw.clone())
            .map_err(|_| crate::err::Error::new(format!("{}: not UTF-8", path.display())))?;
        let table = toml::parse(&text)
            .map_err(|e| e.ctx(path.display().to_string()))?;
        let origin = path.display().to_string();
        let mut r = Reader::new(&origin, &table);

        let name = r.str_req("name")?.to_string();
        let version = r.str_req("version")?.to_string();

        let kind = match r.str_req("kind")? {
            "host-tool" => Kind::HostTool,
            "target" => Kind::Target,
            other => bail!("{origin}: kind `{other}` is not `host-tool` or `target`"),
        };

        let source = match r.table_opt("source")? {
            None => None,
            Some(mut s) => {
                let url = s.str_req("url")?.to_string();
                let sha256 = s.str_req("sha256")?.to_string();
                if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                    bail!("{origin}: sha256 must be 64 hex characters");
                }
                let license = s.str_req("license")?.trim().to_string();
                if license.is_empty() {
                    bail!("{origin}: license must be an SPDX expression");
                }
                let strip = match s.int_opt("strip")? {
                    Some(n) if n < 0 => bail!("{origin}: strip cannot be negative"),
                    Some(n) => n as u32,
                    None => 1,
                };
                s.finish()?;
                Some(Source { url, sha256, license, strip })
            }
        };

        let build = {
            let Some(mut b) = r.table_opt("build")? else {
                bail!("{origin}: a [build] table is required")
            };
            let system = match b.str_req("system")? {
                "configure" => System::Configure,
                "make" => System::Make,
                "shell" => System::Shell,
                other => bail!("{origin}: build system `{other}` is not configure, make or shell"),
            };
            let out_of_tree = b.bool_or("out_of_tree", true)?;
            let configure = b.strs("configure")?;
            let make = b.strs_opt("make")?;
            let install = b.strs_opt("install")?;
            let script = b.str_opt("script")?.map(str::to_string);
            let remove = b.strs("remove")?;
            for p in &remove {
                let escapes = p.starts_with('/') || p.split('/').any(|c| c.is_empty() || c == "..");
                if escapes {
                    bail!("{origin}: remove = `{p}` is not a plain path relative to $OUT");
                }
            }
            let env = match b.table_opt("env")? {
                Some(mut e) => {
                    let pairs = e.pairs()?;
                    e.finish()?;
                    pairs
                }
                None => Vec::new(),
            };
            b.finish()?;

            match system {
                System::Shell if script.is_none() => {
                    bail!("{origin}: system = \"shell\" needs a `script`")
                }
                System::Shell if !configure.is_empty() || make.is_some() => {
                    bail!("{origin}: system = \"shell\" ignores `configure` and `make`; put it all in `script`")
                }
                // a valid shell recipe: everything it does is in the script
                System::Shell => {}
                _ if script.is_some() => {
                    bail!("{origin}: `script` is only for system = \"shell\"")
                }
                System::Make if !configure.is_empty() => {
                    bail!("{origin}: system = \"make\" has no configure step")
                }
                // Without a source there is no tree to configure or make in,
                // so the only thing a recipe can do is say what it creates.
                _ if source.is_none() && system != System::Shell => {
                    bail!("{origin}: a recipe with no [source] must use system = \"shell\"")
                }
                _ => {}
            }

            Build {
                system,
                out_of_tree,
                configure,
                make,
                install,
                env,
                script,
                remove,
            }
        };

        let deps = match r.table_opt("deps")? {
            Some(mut d) => {
                let build = d.strs("build")?;
                let runtime = d.strs("runtime")?;
                d.finish()?;
                Deps { build, runtime }
            }
            None => Deps {
                build: Vec::new(),
                runtime: Vec::new(),
            },
        };

        let checks = match r.table_opt("check")? {
            Some(mut c) => {
                let run = c.strs("run")?;
                c.finish()?;
                if run.is_empty() {
                    bail!("{origin}: a [check] table needs a non-empty `run` list");
                }
                if let Some(bad) = run.iter().find(|c| c.contains('\t') || c.contains('\n')) {
                    bail!("{origin}: check `{bad}` contains a tab or newline; one command per entry");
                }
                run
            }
            None => Vec::new(),
        };

        r.finish()?;

        // Recipe arguments are emitted into a bash script inside double
        // quotes, so $VARS expand. A quote or a backtick would escape that
        // context; refusing at the boundary beats mangling silently.
        for arg in build
            .configure
            .iter()
            .chain(build.make.iter().flatten())
            .chain(build.install.iter().flatten())
            .chain(build.remove.iter())
            .chain(build.env.iter().map(|(_, v)| v))
        {
            if let Some(bad) = arg.chars().find(|c| matches!(c, '"' | '`')) {
                bail!("{origin}: `{arg}` contains {bad:?}, which the generated script cannot quote");
            }
        }

        // A recipe whose name does not match its file is a recipe that gets
        // edited in one place and built from another.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem != name {
            bail!("{origin}: name is `{name}` but the file is `{stem}.toml`");
        }

        Ok(Recipe {
            name,
            version,
            kind,
            source,
            build,
            deps,
            checks,
            path: path.to_path_buf(),
            raw,
        })
    }

    /// Does this recipe read the target's kernel config fragment? Only such a
    /// recipe is rebuilt when the fragment changes.
    pub fn reads_kernel_config(&self) -> bool {
        String::from_utf8_lossy(&self.raw).contains(crate::build::KERNEL_CONFIG_VAR)
    }

    /// Every dependency, in one list. Order is build then runtime.
    pub fn all_deps(&self) -> impl Iterator<Item = &str> {
        self.deps
            .build
            .iter()
            .chain(self.deps.runtime.iter())
            .map(String::as_str)
    }

}

/// Load every `recipes/*.toml`, keyed by name.
pub fn load_all(dir: &Path) -> Result<std::collections::BTreeMap<String, Recipe>> {
    let mut out = std::collections::BTreeMap::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| crate::err::Error::new(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    entries.sort();
    for path in entries {
        let recipe = Recipe::load(&path)?;
        out.insert(recipe.name.clone(), recipe);
    }
    if out.is_empty() {
        bail!("{}: no recipes found", dir.display());
    }
    Ok(out)
}

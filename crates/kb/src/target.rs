//! What a target is.
//!
//! Everything that differs between the `x86_64` cloud image and the `aarch64`
//! headless one is declared here, and nowhere else. Gate 0 criterion 3 is
//! exactly the claim that this file is the only difference.

use crate::err::{Result, bail};
use crate::read::Reader;
use crate::toml;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Boot {
    pub machine: String,
    pub cpu: Option<String>,
    /// kernel name for the serial console, e.g. ttyS0
    pub console: String,
}

#[derive(Debug)]
pub struct Target {
    pub name: String,
    /// The triple we cross-compile to, e.g. `x86_64-koompi-linux-gnu`.
    pub triple: String,
    /// The processor family, as the toolchain spells it.
    pub arch: String,
    /// The processor family, as kbuild spells it. Not always the same.
    pub kernel_arch: String,
    /// repo-relative fragment merged over `make defconfig`
    pub kernel_config: PathBuf,
    /// what an image is assembled from, before runtime dependencies
    pub contents: Vec<String>,
    /// DESIGN.md C5: hardening is policy, and policy that names an
    /// architecture lives here
    pub cflags: Vec<String>,
    pub ldflags: Vec<String>,
    /// DESIGN.md C6: the only files allowed to be setuid or setgid
    pub setuid: Vec<String>,
    pub boot: Boot,
    kernel_config_digest: String,
}

impl Target {
    pub fn load(root: &Path, name: &str) -> Result<Target> {
        let dir = root.join("targets");
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            let known = list(&dir).unwrap_or_default().join(", ");
            bail!("no target `{name}`; known targets: {known}");
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| crate::err::Error::new(format!("{}: {e}", path.display())))?;
        let table = toml::parse(&text).map_err(|e| e.ctx(path.display().to_string()))?;
        let origin = path.display().to_string();
        let mut r = Reader::new(&origin, &table);

        let name_field = r.str_req("name")?.to_string();
        let triple = r.str_req("triple")?.to_string();
        let arch = r.str_req("arch")?.to_string();
        let kernel_arch = r.str_req("kernel_arch")?.to_string();
        let kernel_config = PathBuf::from(r.str_req("kernel_config")?);
        let contents = r.strs("contents")?;
        let cflags = r.strs("cflags")?;
        let ldflags = r.strs("ldflags")?;
        let setuid = r.strs("setuid")?;
        let boot = {
            let Some(mut b) = r.table_opt("boot")? else {
                bail!("{origin}: a [boot] table is required")
            };
            let machine = b.str_req("machine")?.to_string();
            let cpu = b.str_opt("cpu")?.map(str::to_string);
            let console = b.str_req("console")?.to_string();
            b.finish()?;
            Boot { machine, cpu, console }
        };
        r.finish()?;

        if name_field != name {
            bail!("{origin}: name is `{name_field}` but the file is `{name}.toml`");
        }
        if kernel_config.is_absolute() {
            bail!("{origin}: kernel_config must be relative to the repository");
        }
        let config_path = root.join(&kernel_config);
        let config = fs::read(&config_path).map_err(|e| {
            crate::err::Error::new(format!("{origin}: kernel_config {}: {e}", config_path.display()))
        })?;

        Ok(Target {
            name: name_field,
            triple,
            arch,
            kernel_arch,
            kernel_config,
            contents,
            cflags,
            ldflags,
            setuid,
            boot,
            kernel_config_digest: crate::sha256::digest(&config),
        })
    }

    /// The fields a build depends on. Deliberately not the whole file: adding
    /// a package to an image must not invalidate the toolchain.
    pub fn build_identity(&self) -> String {
        format!(
            "{}\n{}\n{}\ncflags {}\nldflags {}\n",
            self.triple,
            self.arch,
            self.kernel_arch,
            self.cflags.join(" "),
            self.ldflags.join(" ")
        )
    }

    /// out of `build_identity` so a fragment edit rebuilds the kernel, not the toolchain
    pub fn kernel_identity(&self) -> &str {
        &self.kernel_config_digest
    }

    /// Every literal a recipe is forbidden to contain.
    pub fn reserved_tokens(&self) -> Vec<&str> {
        vec![&self.triple, &self.arch, &self.kernel_arch]
    }
}

#[cfg(test)]
impl Target {
    pub fn for_test(name: &str, triple: &str, arch: &str, kernel_arch: &str) -> Target {
        Target {
            name: name.into(),
            triple: triple.into(),
            arch: arch.into(),
            kernel_arch: kernel_arch.into(),
            kernel_config: PathBuf::from("config/kernel/test.config"),
            contents: Vec::new(),
            cflags: vec!["-D_FORTIFY_SOURCE=3".into()],
            ldflags: vec!["-Wl,-z,relro,-z,now".into()],
            setuid: Vec::new(),
            boot: Boot {
                machine: "q35".into(),
                cpu: None,
                console: "ttyS0".into(),
            },
            kernel_config_digest: "0".repeat(64),
        }
    }
}

pub fn list(dir: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| crate::err::Error::new(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

pub fn load_all(root: &Path) -> Result<Vec<Target>> {
    list(&root.join("targets"))?
        .iter()
        .map(|n| Target::load(root, n))
        .collect()
}

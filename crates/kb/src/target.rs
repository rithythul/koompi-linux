//! What a target is.
//!
//! Everything that differs between the `x86_64` cloud image and the `aarch64`
//! headless one is declared here, and nowhere else. Gate 0 criterion 3 is
//! exactly the claim that this file is the only difference.

use crate::err::{Result, bail};
use crate::read::Reader;
use crate::toml;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub struct Target {
    pub name: String,
    /// The triple we cross-compile to, e.g. `x86_64-koompi-linux-gnu`.
    pub triple: String,
    /// The processor family, as the toolchain spells it.
    pub arch: String,
    /// The processor family, as kbuild spells it. Not always the same.
    pub kernel_arch: String,
}

impl Target {
    pub fn load(dir: &Path, name: &str) -> Result<Target> {
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            let known = list(dir).unwrap_or_default().join(", ");
            bail!("no target `{name}`; known targets: {known}");
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| crate::err::Error::new(format!("{}: {e}", path.display())))?;
        let table = toml::parse(&text).map_err(|e| e.ctx(path.display().to_string()))?;
        let origin = path.display().to_string();
        let mut r = Reader::new(&origin, &table);

        let target = Target {
            name: r.str_req("name")?.to_string(),
            triple: r.str_req("triple")?.to_string(),
            arch: r.str_req("arch")?.to_string(),
            kernel_arch: r.str_req("kernel_arch")?.to_string(),
        };
        r.finish()?;

        if target.name != name {
            bail!("{origin}: name is `{}` but the file is `{name}.toml`", target.name);
        }
        Ok(target)
    }

    /// The fields a build depends on. Deliberately not the whole file: adding
    /// a package to an image must not invalidate the toolchain.
    pub fn build_identity(&self) -> String {
        format!("{}\n{}\n{}\n", self.triple, self.arch, self.kernel_arch)
    }

    /// Every literal a recipe is forbidden to contain.
    pub fn reserved_tokens(&self) -> Vec<&str> {
        vec![&self.triple, &self.arch, &self.kernel_arch]
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

pub fn load_all(dir: &Path) -> Result<Vec<Target>> {
    list(dir)?.iter().map(|n| Target::load(dir, n)).collect()
}

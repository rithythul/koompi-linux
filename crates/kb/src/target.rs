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
    /// DESIGN.md C4: fragment names under config/kernel/, merged in order
    /// over `make defconfig`; a later fragment overrides an earlier line
    pub kernel_fragments: Vec<String>,
    /// the merged fragments, what the linux recipe reads as $KERNEL_CONFIG
    pub kernel_config: String,
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
        let kernel_fragments = r.strs("kernel_config")?;
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
        if kernel_fragments.is_empty() {
            bail!("{origin}: kernel_config must list at least one fragment from config/kernel/");
        }
        let mut fragments = Vec::new();
        for name in &kernel_fragments {
            if !is_fragment_name(name) {
                bail!("{origin}: kernel_config `{name}` is not a fragment name (a-z, 0-9, - and _)");
            }
            let path = fragment_path(root, name);
            let text = fs::read_to_string(&path).map_err(|e| {
                crate::err::Error::new(format!("{origin}: kernel_config `{name}`: {}: {e}", path.display()))
            })?;
            fragments.push((name.as_str(), text));
        }
        let kernel_config = merge_fragments(&fragments).map_err(|e| e.ctx(origin.clone()))?;

        Ok(Target {
            name: name_field,
            triple,
            arch,
            kernel_arch,
            kernel_fragments,
            kernel_config_digest: crate::sha256::digest(kernel_config.as_bytes()),
            kernel_config,
            contents,
            cflags,
            ldflags,
            setuid,
            boot,
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

pub fn fragment_path(root: &Path, name: &str) -> PathBuf {
    root.join("config/kernel").join(format!("{name}.config"))
}

fn is_fragment_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The symbol a kernel config line sets, or None for a blank or comment
/// line. A line that is neither is not a kernel config line.
fn config_symbol(line: &str) -> Result<Option<&str>> {
    if line.is_empty() {
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("# ") {
        if let Some(sym) = rest.strip_suffix(" is not set")
            && sym.starts_with("CONFIG_")
        {
            return Ok(Some(sym));
        }
        return Ok(None);
    }
    if line == "#" {
        return Ok(None);
    }
    if let Some((sym, _)) = line.split_once('=')
        && sym.starts_with("CONFIG_")
    {
        return Ok(Some(sym));
    }
    bail!("`{line}` is not a kernel config line")
}

/// Fragments in order, later overriding earlier by symbol. The result is
/// exactly the set of lines the linux recipe demands survive olddefconfig,
/// so an override does not read as a loss. Comments do not carry over:
/// the fragments are the readable thing, this is the mounted thing.
pub fn merge_fragments(fragments: &[(&str, String)]) -> Result<String> {
    let mut lines: Vec<(String, String)> = Vec::new();
    for (name, text) in fragments {
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim_end();
            let Some(sym) = config_symbol(line).map_err(|e| e.ctx(format!("config/kernel/{name}.config:{}", n + 1)))? else {
                continue;
            };
            lines.retain(|(s, _)| s != sym);
            lines.push((sym.to_string(), line.to_string()));
        }
    }
    let mut out = String::new();
    for (_, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
impl Target {
    pub fn for_test(name: &str, triple: &str, arch: &str, kernel_arch: &str) -> Target {
        Target {
            name: name.into(),
            triple: triple.into(),
            arch: arch.into(),
            kernel_arch: kernel_arch.into(),
            kernel_fragments: vec!["test".into()],
            kernel_config: "CONFIG_TEST=y\n".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_fragment_overrides_an_earlier_line_by_symbol() {
        let merged = merge_fragments(&[
            ("core", "# what every kernel is\nCONFIG_A=y\n# CONFIG_B is not set\nCONFIG_C=\"x\"\n".into()),
            ("target", "\nCONFIG_B=m\nCONFIG_D=y\n".into()),
        ])
        .unwrap();
        assert_eq!(merged, "CONFIG_A=y\nCONFIG_C=\"x\"\nCONFIG_B=m\nCONFIG_D=y\n");
    }

    #[test]
    fn a_line_that_is_not_a_kernel_config_line_is_refused() {
        let err = merge_fragments(&[("core", "CONFIG_A=y\nCONFIG_B\n".into())]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("config/kernel/core.config:2"), "{msg}");
        assert!(msg.contains("`CONFIG_B` is not a kernel config line"), "{msg}");
    }

    #[test]
    fn fragment_names_are_plain() {
        assert!(is_fragment_name("x86_64-cloud"));
        assert!(!is_fragment_name("../core"));
        assert!(!is_fragment_name(""));
        assert!(!is_fragment_name("Core"));
    }
}

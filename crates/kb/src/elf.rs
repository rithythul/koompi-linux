//! Just enough ELF to answer two questions about a file in an image: which
//! loader does it ask for, and which libraries does it name.
//!
//! Reading it here rather than shelling out to `readelf` keeps the provenance
//! gate a pure function of the image, which is what lets it be a test.

use crate::err::{Error, Result};

const MAGIC: &[u8] = b"\x7fELF";
const CLASS_64: u8 = 2;
const LITTLE_ENDIAN: u8 = 1;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

pub struct Elf {
    pub interpreter: Option<String>,
    pub needed: Vec<String>,
    /// Both `DT_RPATH` and `DT_RUNPATH`; where the loader is told to search.
    pub run_paths: Vec<String>,
}

pub fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

struct Segment {
    kind: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
}

pub fn read(bytes: &[u8]) -> Result<Elf> {
    if !is_elf(bytes) {
        return Err(Error::new("not an ELF file"));
    }
    if bytes.len() < 64 {
        return Err(Error::new("truncated ELF header"));
    }
    if bytes[4] != CLASS_64 {
        return Err(Error::new("only 64-bit ELF is understood; both targets are 64-bit"));
    }
    if bytes[5] != LITTLE_ENDIAN {
        return Err(Error::new("only little-endian ELF is understood"));
    }

    let phoff = u64_at(bytes, 32)?;
    let phentsize = u16_at(bytes, 54)? as usize;
    let phnum = u16_at(bytes, 56)? as usize;
    // a relocatable object (a kernel module) has no program headers at all
    if phnum > 0 && phentsize < 56 {
        return Err(Error::new("program header entries are too small to be ELF64"));
    }

    let mut segments = Vec::new();
    for n in 0..phnum {
        let at = phoff as usize + n * phentsize;
        segments.push(Segment {
            kind: u32_at(bytes, at)?,
            offset: u64_at(bytes, at + 8)?,
            vaddr: u64_at(bytes, at + 16)?,
            filesz: u64_at(bytes, at + 32)?,
        });
    }

    let interpreter = match segments.iter().find(|s| s.kind == PT_INTERP) {
        Some(s) => Some(string_at(bytes, s.offset as usize)?),
        None => None,
    };

    let mut needed = Vec::new();
    let mut run_paths = Vec::new();
    if let Some(dynamic) = segments.iter().find(|s| s.kind == PT_DYNAMIC) {
        let mut entries = Vec::new();
        let mut strtab = None;
        let mut at = dynamic.offset as usize;
        let end = at + dynamic.filesz as usize;
        while at + 16 <= end {
            let tag = u64_at(bytes, at)?;
            let val = u64_at(bytes, at + 8)?;
            if tag == DT_NULL {
                break;
            }
            if tag == DT_STRTAB {
                strtab = Some(val);
            }
            if matches!(tag, DT_NEEDED | DT_RPATH | DT_RUNPATH) {
                entries.push((tag, val));
            }
            at += 16;
        }
        // DT_STRTAB is an address, and only the load segments say where an
        // address lives in the file.
        if let Some(strtab) = strtab {
            let base = file_offset(&segments, strtab)
                .ok_or_else(|| Error::new("the dynamic string table is in no loaded segment"))?;
            for (tag, val) in entries {
                let s = string_at(bytes, base + val as usize)?;
                if tag == DT_NEEDED {
                    needed.push(s);
                } else {
                    run_paths.extend(s.split(':').map(str::to_string));
                }
            }
        }
    }

    Ok(Elf { interpreter, needed, run_paths })
}

fn file_offset(segments: &[Segment], vaddr: u64) -> Option<usize> {
    segments
        .iter()
        .find(|s| s.kind == PT_LOAD && vaddr >= s.vaddr && vaddr < s.vaddr + s.filesz)
        .map(|s| (s.offset + (vaddr - s.vaddr)) as usize)
}

fn string_at(bytes: &[u8], at: usize) -> Result<String> {
    let rest = bytes.get(at..).ok_or_else(|| Error::new("a string points past the end of the file"))?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    String::from_utf8(rest[..end].to_vec()).map_err(|_| Error::new("a string is not UTF-8"))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    let b = bytes.get(at..at + 2).ok_or_else(|| Error::new("truncated ELF"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let b = bytes.get(at..at + 4).ok_or_else(|| Error::new("truncated ELF"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
    let b = bytes.get(at..at + 8).ok_or_else(|| Error::new("truncated ELF"))?;
    Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary running this test: built by another toolchain entirely,
    /// which is the point — the reader must not know whose ELF it is.
    fn this_binary() -> Vec<u8> {
        std::fs::read(std::env::current_exe().unwrap()).unwrap()
    }

    #[test]
    fn reads_the_interpreter_and_libraries_of_a_real_binary() {
        let bytes = this_binary();
        let elf = read(&bytes).unwrap();
        let interp = elf.interpreter.expect("a dynamic test binary has an interpreter");
        assert!(interp.starts_with('/'), "{interp}");
        assert!(interp.contains("ld-"), "{interp}");
        assert!(
            elf.needed.iter().any(|n| n.starts_with("libc.so")),
            "{:?}",
            elf.needed
        );
    }

    #[test]
    fn rubbish_is_rejected_rather_than_guessed_at() {
        assert!(!is_elf(b"#!/bin/sh\n"));
        assert!(read(b"#!/bin/sh\n").is_err());
        assert!(read(b"\x7fELF").is_err());
    }
}

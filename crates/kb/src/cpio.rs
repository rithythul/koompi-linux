//! The `newc` cpio archive an initramfs is.
//!
//! Written here rather than shelled out to because the image needs
//! `/dev/console` — without it the kernel gives PID 1 no stdio — and a
//! process in a user namespace may not create a device node. Building the
//! archive ourselves is also how every entry ends up owned by root with a
//! fixed timestamp, whoever ran the build.

use crate::err::{Error, Result};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

const MAGIC: &[u8] = b"070701";
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;

pub struct Archive<W: Write> {
    out: W,
    ino: u32,
    bytes: u64,
}

impl<W: Write> Archive<W> {
    pub fn new(out: W) -> Archive<W> {
        Archive { out, ino: 1, bytes: 0 }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.out.write_all(data)?;
        self.bytes += data.len() as u64;
        Ok(())
    }

    fn pad_to_four(&mut self) -> Result<()> {
        let pad = (4 - (self.bytes % 4) as usize) % 4;
        self.write_all(&b"\0\0\0"[..pad])
    }

    fn header(&mut self, name: &str, mode: u32, size: u64, rdev: (u32, u32)) -> Result<()> {
        if name.contains('\0') {
            return Err(Error::new(format!("{name}: a path with a NUL cannot be archived")));
        }
        let ino = self.ino;
        self.ino += 1;
        let fields = [
            ino,
            mode,
            0, // uid
            0, // gid
            1, // nlink
            0, // mtime, fixed so the same rootfs gives the same archive
            u32::try_from(size)
                .map_err(|_| Error::new(format!("{name}: newc cpio cannot hold a file over 4 GiB")))?,
            0, // devmajor
            0, // devminor
            rdev.0,
            rdev.1,
            name.len() as u32 + 1,
            0, // check, unused by the newc format
        ];
        self.write_all(MAGIC)?;
        for f in fields {
            self.write_all(format!("{f:08x}").as_bytes())?;
        }
        self.write_all(name.as_bytes())?;
        self.write_all(b"\0")?;
        self.pad_to_four()
    }

    fn entry(&mut self, name: &str, mode: u32, data: &[u8]) -> Result<()> {
        self.header(name, mode, data.len() as u64, (0, 0))?;
        self.write_all(data)?;
        self.pad_to_four()
    }

    pub fn directory(&mut self, name: &str, mode: u32) -> Result<()> {
        self.entry(name, S_IFDIR | mode, &[])
    }

    pub fn symlink(&mut self, name: &str, target: &str) -> Result<()> {
        self.entry(name, S_IFLNK | 0o777, target.as_bytes())
    }

    pub fn char_device(&mut self, name: &str, mode: u32, major: u32, minor: u32) -> Result<()> {
        self.header(name, S_IFCHR | mode, 0, (major, minor))
    }

    pub fn file(&mut self, name: &str, mode: u32, path: &Path) -> Result<()> {
        let size = fs::metadata(path)?.len();
        self.header(name, S_IFREG | mode, size, (0, 0))?;
        let mut f = fs::File::open(path)?;
        let mut buf = vec![0u8; 1 << 16];
        let mut left = size;
        while left > 0 {
            let want = buf.len().min(left as usize);
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                return Err(Error::new(format!(
                    "{}: shrank while it was being archived",
                    path.display()
                )));
            }
            self.write_all(&buf[..n])?;
            left -= n as u64;
        }
        self.pad_to_four()
    }

    /// The end-of-archive marker. Without it the kernel reports a corrupt
    /// initramfs and boots to a panic.
    pub fn finish(mut self) -> Result<u64> {
        self.header("TRAILER!!!", 0, 0, (0, 0))?;
        self.out.flush()?;
        Ok(self.bytes)
    }
}

/// Archive `root` in full, depth first and sorted, so the same tree always
/// produces the same bytes.
pub fn append_tree<W: Write>(archive: &mut Archive<W>, root: &Path, prefix: &str) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| Error::new(format!("{}: {e}", root.display())))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let meta = entry.metadata()?;
        let mode = meta.permissions().mode() & 0o7777;
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            archive.symlink(&name, &target.to_string_lossy())?;
        } else if file_type.is_dir() {
            archive.directory(&name, mode)?;
            append_tree(archive, &path, &name)?;
        } else if file_type.is_file() {
            archive.file(&name, mode, &path)?;
        } else if file_type.is_char_device() || file_type.is_block_device() {
            return Err(Error::new(format!(
                "{}: a device node in the store, which no build can create",
                path.display()
            )));
        } else {
            return Err(Error::new(format!(
                "{}: a socket or fifo cannot be part of an image",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(bytes: &[u8], n: usize) -> u32 {
        let at = 6 + n * 8;
        u32::from_str_radix(std::str::from_utf8(&bytes[at..at + 8]).unwrap(), 16).unwrap()
    }

    #[test]
    fn every_header_starts_on_a_four_byte_boundary() {
        let mut out = Vec::new();
        let mut a = Archive::new(&mut out);
        a.directory("dev", 0o755).unwrap();
        a.symlink("bin", "usr/bin").unwrap();
        a.char_device("dev/console", 0o600, 5, 1).unwrap();
        a.finish().unwrap();

        let mut at = 0;
        let mut seen = 0;
        while at + 110 <= out.len() {
            assert_eq!(at % 4, 0, "header {seen} starts at {at}");
            assert_eq!(&out[at..at + 6], MAGIC);
            let size = field(&out[at..], 6) as usize;
            let namesize = field(&out[at..], 11) as usize;
            at += 110 + namesize;
            at += (4 - at % 4) % 4;
            at += size;
            at += (4 - at % 4) % 4;
            seen += 1;
        }
        assert_eq!(seen, 4, "three entries and a trailer");
    }

    #[test]
    fn a_device_node_carries_its_numbers_and_nothing_else() {
        let mut out = Vec::new();
        let mut a = Archive::new(&mut out);
        a.char_device("dev/console", 0o600, 5, 1).unwrap();
        a.finish().unwrap();
        assert_eq!(field(&out, 1) & 0o170000, S_IFCHR);
        assert_eq!(field(&out, 1) & 0o7777, 0o600);
        assert_eq!(field(&out, 6), 0, "a device node has no data");
        assert_eq!(field(&out, 9), 5);
        assert_eq!(field(&out, 10), 1);
    }

    /// An independent parser reading a whole tree back: the kernel is the
    /// real consumer, and a wrong archive there is a silent boot.
    #[test]
    fn a_tree_round_trips_through_an_independent_cpio() {
        let dir = std::env::temp_dir().join(format!("kb-cpio-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        fs::write(dir.join("usr/bin/sh"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("usr/bin", dir.join("bin")).unwrap();

        let mut out = Vec::new();
        let mut a = Archive::new(&mut out);
        append_tree(&mut a, &dir, "").unwrap();
        a.char_device("dev/console", 0o600, 5, 1).unwrap();
        a.finish().unwrap();
        fs::remove_dir_all(&dir).unwrap();

        let Ok(mut child) = std::process::Command::new("cpio")
            .args(["-t", "--quiet"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("no cpio on this machine; the round trip was not checked");
            return;
        };
        child.stdin.take().unwrap().write_all(&out).unwrap();
        let listed = child.wait_with_output().unwrap();
        assert!(listed.status.success(), "cpio rejected the archive");
        let names: Vec<&str> = std::str::from_utf8(&listed.stdout).unwrap().lines().collect();
        assert_eq!(names, ["bin", "usr", "usr/bin", "usr/bin/sh", "dev/console"]);
    }

    #[test]
    fn everything_is_owned_by_root() {
        let mut out = Vec::new();
        let mut a = Archive::new(&mut out);
        a.directory("root", 0o700).unwrap();
        a.finish().unwrap();
        assert_eq!(field(&out, 2), 0, "uid");
        assert_eq!(field(&out, 3), 0, "gid");
    }
}

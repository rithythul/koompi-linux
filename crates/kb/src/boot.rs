//! Booting an image in QEMU.
//!
//! M2 boots with `-kernel` and no firmware on purpose: with the boot chain
//! out of the way, anything that fails is the userland, which is the part
//! being built. The boot chain arrives in M3 and gets its own failures.

use crate::build::Engine;
use crate::err::{Error, Result, bail};
use crate::image::Image;
use crate::target::Target;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What the selftest prints when it has run every check and they all passed.
const PASSED: &str = "SELFTEST PASSED";
const FAILED: &str = "SELFTEST FAILED";

pub struct Options {
    pub smoke: bool,
    pub timeout: Duration,
    pub memory_mb: u32,
}

pub fn run(engine: &Engine, target: &Target, options: &Options) -> Result<()> {
    let image = Image::of(engine, target);
    if !image.exists() {
        bail!("no image for {}\n  run: kb image {}", target.name, target.name);
    }

    let qemu = format!("qemu-system-{}", target.arch);
    let mut cmdline = format!("console={} panic=10", target.boot.console);
    if options.smoke {
        cmdline.push_str(" koompi.selftest");
    }

    let mut command = Command::new(&qemu);
    command
        .args(["-machine", &target.boot.machine])
        .args(["-m", &options.memory_mb.to_string()])
        .arg("-no-reboot")
        .args(["-kernel", &image.kernel.to_string_lossy()])
        .args(["-initrd", &image.initramfs.to_string_lossy()])
        .args(["-append", &cmdline]);
    if let Some(cpu) = &target.boot.cpu {
        command.args(["-cpu", cpu]);
    }
    if can_accelerate(target) {
        command.arg("-enable-kvm");
    }

    if !options.smoke {
        command.arg("-nographic");
        println!("boot {} ({qemu}); leave with ctrl-a x", target.name);
        let status = command.status().map_err(|e| missing(&qemu, e))?;
        return match status.success() {
            true => Ok(()),
            false => bail!("{qemu} exited with {status}"),
        };
    }

    command
        .args(["-display", "none", "-serial", "stdio"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    println!("boot {} ({qemu}), waiting up to {}s for the selftest",
             target.name, options.timeout.as_secs());
    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| missing(&qemu, e))?;
    let stdout = child.stdout.take().expect("stdout was piped");

    let (lines, from_guest) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if lines.send(line).is_err() {
                return;
            }
        }
    });

    let mut verdict = None;
    while verdict.is_none() {
        let left = match options.timeout.checked_sub(started.elapsed()) {
            Some(left) if !left.is_zero() => left,
            _ => break,
        };
        match from_guest.recv_timeout(left) {
            Ok(line) => {
                println!("  | {line}");
                if line.contains(PASSED) {
                    verdict = Some(true);
                } else if line.contains(FAILED) {
                    verdict = Some(false);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            // QEMU closed the console: the guest is gone, and whatever it was
            // going to say it has said.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let secs = started.elapsed().as_secs();

    match verdict {
        Some(true) => {
            println!("boot {}: selftest passed in {secs}s", target.name);
            Ok(())
        }
        Some(false) => bail!("{}: the guest booted and its selftest failed", target.name),
        None => bail!(
            "{}: no selftest result after {secs}s\n  \
             the lines above are everything the guest said",
            target.name
        ),
    }
}

/// KVM only where the guest is the machine we are on; anything else is
/// emulation, and asking for acceleration there fails rather than falls back.
fn can_accelerate(target: &Target) -> bool {
    target.arch == std::env::consts::ARCH
        && std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/kvm")
            .is_ok()
}

fn missing(qemu: &str, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        return Error::new(format!("{qemu} is not installed; it is what boots the image"));
    }
    Error::new(format!("running {qemu}: {e}"))
}

# What the first three attempts cost, and what they proved

Written 2026-09-01, immediately before deleting `koompi-linux` and `koompi-os`.
The code is gone deliberately. This file exists so the fourth attempt does not re-derive what the first three already paid for.

Nothing here is a decision. It is evidence.

## The three attempts

### 1. Base-system layer — own toolchain (deleted 2026-08-31, 89 issues)

Its own `x86_64-koompi-linux-gnu` triple, a four-stage bootstrap, 214 recipes built from source.

**It worked further than expected.** It reached a self-hosting seed and booted to a login prompt in QEMU.

**It died of throughput, not of any single defect.** It stalled partway through the package set, with gate 1 at 44 of 149 rows (39 ok, 5 FAIL).
The open blockers when it stopped were ordinary and individually solvable:

- no `elfutils` recipe, so the kernel could not build — `objtool` needs `gelf.h`, pulled in by `CONFIG_UNWINDER_ORC=y`
- the kbuild dependency-order planner cut a *runtime* edge to break a cycle, on the assumption the chroot already had it; over a minimal seed it did not
- `efitools` calls `strptime` with no feature test — glibc guards it behind `__USE_XOPEN`, musl did not
- seven meson recipes could fetch undeclared subprojects, failing closed only because the build chroot had no network

**It changed libc mid-flight**, musl to glibc, and paid for it in flag-day breakage of exactly the `strptime` kind.

**One machine was the bottleneck** — 8 cores at load 18. A second concurrent build made `-j` a live variable in what both produced.

### 2. `koompi-os` — Debian (2026-08-19 to 2026-08-27, 64 commits, 8 days)

Debian trixie, systemd, mutable root, btrfs with snapper and grub-btrfs for rollback.
Editions as profiles layered over one base, never forks.
`koompi` as the only user-facing package interface, with apt, flatpak and distrobox underneath.
KDE Plasma, plus an M0 scaffold of an own compositor on Smithay.
Khmer fonts and input method packaged.

It was not defeated. It was superseded, eight days in, by a decision about which layer to own.

### 3. `koompi-linux` — distribution layer over Arch (2026-08-31 to 2026-09-01, 9 commits)

26 architecture decisions resolved on an issue-tracker map.
Two working tools: a routing dispatcher and an atomic image-apply engine.
It never produced a package or an image.

Superseded, two days in, by a decision about scope.

## The pattern, stated plainly

Three restarts in fourteen days.
Attempt 1 died of throughput; attempts 2 and 3 died of decisions made *after* building rather than before.

The lesson is not "do not restart".
It is that the layer and scope questions have to be answered once, up front, and held — which is the entire reason the fourth attempt starts with a spec session instead of a build.

## The number that decides the fourth attempt

Attempt 1 stalled at 214 recipes, written by hand.
That is the baseline.

If agent-driven recipe throughput is not an order of magnitude better than that, the fourth attempt ends the same way, and it should be possible to know this in week two rather than month six.

## Findings worth carrying forward

These were measured, not assumed, and none of them depend on which layer we build at.

### Kernel

- One kernel, `linux-lts` on the newest longterm. There is no 7.x LTS.
- `CONFIG_MODVERSIONS=y` **from the very first build**, or every module published before it is invalidated later.
- Arch's kernel does **not** set `CONFIG_MODVERSIONS`, and signs modules with a key it generates per build and discards (`CONFIG_MODULE_SIG_KEY="certs/signing_key.pem"`). You therefore cannot take Arch's kernel binary and sign your own out-of-tree modules against it.
- Arch builds with `CONFIG_DEBUG_INFO=y`, DWARF5 and BTF. That is most of the ~26 GB kernel build tree, and it is scratch, not storage — it is deleted after each build and never accumulates.

### Boot and the ESP

- **The ESP write is the one failure no snapshot recovers.** Write to a hidden staging name that does not end in `.efi` (systemd-boot scans the directory and would offer a half-written kernel), fsync, read back and compare byte for byte, rename into place, fsync the directory. Never overwrite an existing entry. Retain 3. Never collect before a successful boot.
- **Unattended rollback needs no daemon.** systemd-boot counts tries in the entry *filename* (`name+3.efi` → `+2-1` → `+0-3`), and `systemd-bless-boot.service` clears the counter after a good boot. Do not write this.
- A Unified Kernel Image per deployment, with a permanent recovery entry (`nomodeset`, no proprietary driver) in every image, since the cmdline is baked in.

### Size, measured

- A desktop base closed over dependencies is **354 packages, 1.43 GiB compressed, 3.59 GiB installed** — smaller than Arch's `core` (297 packages, 1.8 GiB) and a fiftieth of `extra` (14,947 packages, 105.7 GiB).
- Build working space is roughly 40 GB and does not grow. A deduplicated archive with a retention window is roughly 20 GB/year.
- **Storage is not a reason to buy a machine.** CPU time is the only real cost.

### Security process

- 85 open Arch advisories, 11 Critical/High, **0 with a fix available**. When you build from upstream recipes, an advisory with no fix is no work — a drift bot that rebuilds on upstream change *is* the CVE response, and the advisory feed audits the bot rather than generating work.

### Khmer

- **Khmer line breaking is broken on every Linux**, at Pango 1.58.2 included. A 32-character Khmer sentence yields exactly one break opportunity, at the end of the string; Thai yields seven, through libthai. Pango's `break_script()` has no Khmer case.
- ICU's `khmerdict.dict` already ships inside the freedesktop-sdk runtime, unused. `break-thai.c` is the template. This is the one place where "from Cambodia" could mean an actual upstream patch.
- Coeng clustering is separate and self-resolved: freedesktop-sdk 26.08 pins Pango 1.58.2.

### Packaging traps

- `pacman -S` cannot install into a fresh root — it has no sync databases. `-Sy` can. Never `-Syu`, which upgrades past what the image pinned.
- Point the download cache outside the target root, or it is sealed into the read-only image and never reclaimable.
- `pacman -Fq` exits non-zero both for "no match" and "database missing". Only stderr separates them, and reading the second as the first routes confidently from half a system.

### Compositor

- labwc 0.20.2 and sway both advertise 8/8 of a modern shell's protocol set. Wayfire's surface is plugin-gated and under-reports without five plugins explicitly enabled. niri has no headless backend and is not covered by `xdg-desktop-portal-wlr`.

### Tooling traps that cost real time

- An unanchored `.gitignore` pattern matches at **every depth**. `sources/`, left from a deleted top-level directory, silently swallowed `src/sources/` and produced a commit that did not build while every test passed. **Verify the commit, not the working tree**: `git archive HEAD | tar -x -C "$(mktemp -d)"`, then build there.
- `#[derive(Default)]` on a struct holding loaded configuration silently substitutes an empty one. An allowlist defaulted rather than loaded refused every request, on every machine, and no unit test caught it. Running the real binary did.
- Rootless podman cannot loop-mount btrfs, and `--privileged` does not change that. Real filesystem work needs a root container or a VM.

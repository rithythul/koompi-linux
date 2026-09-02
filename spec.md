# Spec: the KOOMPI Linux core

Stage 2 of the agent-native SDLC, produced in one session on 2026-09-01.
[intent.md](intent.md) says *why*. This says *what*, once.

Attempt three spread this over 26 tickets and several weeks, and was abandoned before any of it was built.
The point of settling it in one sitting is that these answers are then held, not revisited.

## The difference, in a sentence

**The whole core is small enough for one person to read in a day.**

Yocto is powerful and unreadable. Nix is reproducible behind a language wall. Buildroot is neither general nor composable.
Readability is the constraint the rest of this document serves, and the one that makes the agent-throughput bet easier rather than harder — an agent that can hold the entire core in context makes fewer wrong guesses than one that cannot.

It is enforced by a budget, not by intent: see [Size](#size-is-a-constraint-not-an-outcome).

## Decisions

| # | Decision | Because |
|---|---|---|
| 1 | **glibc**, with libc a parameter of the build graph | Desktop and AI targets need it — NVIDIA, CUDA, Electron are glibc-only. Attempt one started musl and paid for the move in flag-day breakage. Parameterising keeps musl reachable without paying for it now. |
| 2 | **Recipes are declarative TOML** with a shell escape hatch | Most packages are metadata plus a known build system; that is data, not code. Machine-checkable, uniform, and the format an agent is least able to get plausibly wrong. |
| 3 | **Cross-first, qemu-user fallback** | aarch64 is target two from day one. Cross by default keeps the common path fast; binfmt covers packages that run their own output at build time. Retrofitting cross-compilation later is what kills these projects. |
| 4 | **dinit as PID 1** | systemd is ~1.5M lines. Shipping it makes decision zero false on the first day, and once it is in it never comes out. |
| 5 | **Image-based atomic updates**; development happens in containers | What cloud, phone, automotive and appliance targets all actually want. Makes a target reproducible rather than merely convergent. |
| 6 | **Seed from a stock toolchain**, then rebuild with itself | "Downstream of no distro" holds: only the seed is borrowed, and it is discarded. Attempt one's four-stage bootstrap is where its time went, and it died before reaching its package set. |
| 7 | **First two targets: `x86_64` cloud and `aarch64` headless** | Dissimilar in architecture *and* userland shape, so they actually test the molding claim rather than demonstrating profiles. |

## Engineering calls, made rather than asked

### systemd-boot stays, systemd does not

Decision 4 appears to cost unattended rollback, which the post-mortem found came free from systemd-boot's boot counting.
It does not.

`systemd-boot` is a standalone UEFI boot manager. It implements boot counting entirely inside the EFI binary, by renaming entries in `EFI/Linux`, and requires nothing of userspace.
The only userspace piece is `systemd-bless-boot`, which removes the counter after a good boot — a single file rename, reimplemented as a dinit oneshot in well under a hundred lines.

So the core builds `systemd-boot` alone from the systemd tree, ships it as the boot manager, and runs dinit as PID 1.
The post-mortem's ESP write protocol and its retain-3 rule apply unchanged.

### Reproducibility is a goal, not yet a gate

Recipes are declarative, so builds are reproducible by construction wherever the format allows.
Bit-identical output is **not** a merge gate before Gate 0 clears.

Making it one now would spend the exact resource — throughput — that the post-mortem identifies as what killed attempt one.
It becomes a gate once the core boots.

### Everything KOOMPI writes is Rust

Carried from attempt three unchanged. The recipe engine, the image builder and the deploy tool are Rust with no external crates unless a dependency-register row justifies one.

### Kernel

One kernel, `linux-lts` on the newest longterm, per target config.
`CONFIG_MODVERSIONS=y` from the first build, or every module published before it is invalidated later.
`CONFIG_DEBUG_INFO` off — we ship no debug packages, and it is most of a 26 GB build tree.

## Size is a constraint, not an outcome

**The `x86_64` cloud target is capped at 150 recipes.**

Not an estimate. A budget. Exceeding it means something belongs in a layer above the core, or does not belong at all.

At 150 recipes, "one person reads the entire core in a day" is roughly three minutes each, which is the test.
For scale, the post-mortem measured a full desktop base at 354 packages; a cloud target has no display stack, no fonts and no desktop session.

## How we know it works

Gate 0, before any commitment beyond it:

1. an `x86_64` cloud image boots in QEMU to a shell, built entirely from our own recipes on a discarded seed toolchain
2. an `aarch64` headless image boots in QEMU
3. both come from **one recipe set**, differing only in a target declaration — no forked recipes
4. throughput is measured in recipes per day, against attempt one's baseline of 214 written by hand

Criteria 3 is the one that matters. One and two without three is a distro; all three is the product.

## What this spec deliberately does not settle

- the desktop, in any form — it starts after Gate 0
- signing, key custody and release infrastructure — blocked on Rithy, and not needed to boot
- whether targets beyond the first two get first-class support
- the on-device story for anything mutable

## What would make this spec wrong

Inherited from intent.md, restated so it is checkable here:

- recipe throughput fails to clear 214 by roughly an order of magnitude
- two dissimilar targets cannot come from one graph without forking recipes
- the 150-recipe budget cannot hold for a booting cloud target

Any of the three, and the answer is to stop and say so.

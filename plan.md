# Plan: reaching Gate 0

Stage 3 of the agent-native SDLC.
[intent.md](intent.md) says why, [spec.md](spec.md) says what, this says in what order, and how each step is known to be done.

Nothing here reopens a spec decision.
Where this plan makes a call the spec left open, it is marked as such and stated once.

## What Gate 0 needs, and what it does not

Gate 0 is four criteria, and criterion 3 is the one that matters.
Everything below is sequenced by which of them it de-risks, not by what is pleasant to build first.

**In:** an `x86_64` cloud image and an `aarch64` headless image, both booting in QEMU to a shell, both from one recipe set with no forked recipes, with throughput measured.

**Out, and deliberately so:** self-hosting.
Spec decision 6 says seed from a stock toolchain and rebuild with itself.
The rebuild-with-itself half is **not** in Gate 0.
Attempt one spent its calendar on a four-stage bootstrap and died before reaching its package set; doing that again, before knowing whether the throughput bet holds, repeats the exact mistake the post-mortem names.
The seed is discarded and provenance is checked (below), which is what "downstream of no distribution" actually requires.
Building a native toolchain and rebuilding the world under it is the first thing after Gate 0, not part of it.

## The shape of the build

### The seed is a container image, pinned by digest

Build-time tools — a host compiler, make, perl, python, elfutils, coreutils — come from one OCI image referenced by digest, never by tag.
It is a scaffold: it builds our cross toolchains and nothing it contains reaches an image.

Concretely: `seed/Containerfile` pins its own base by digest, and the image it produces is pinned by the id in `seed/DIGEST`, which is what `kb` passes to podman.
A local image has no registry digest until it is pushed, so the id is the pin until there is somewhere to push it.

This is a call the spec left open, and it buys two things.
The recipe budget is spent on what ships rather than on autotools.
And "seed" becomes a single pinned artifact that can be swapped or diffed, instead of whatever happens to be installed on the machine.

The build container has **no network**.
Sources are fetched beforehand, verified against a `sha256` in the recipe, and mounted read-only.

### Provenance is checked, not asserted

"Downstream of nobody" is a claim, so it gets a test.

`kb check-provenance` walks every file in a finished image and fails on any of:

- an ELF object whose `DT_NEEDED` or interpreter resolves outside the image
- a build-id not present in the build store
- a seed path (`/usr` of the container) appearing in any binary, script shebang, `.pc` file, or config

It runs as a gate on every image, not as an audit at the end.

### Two targets from the first recipe, not the last

Retrofitting the second target is how criterion 3 fails.
So `aarch64` enters at the toolchain, which is the hardest place, rather than at the image, which is the easiest.

**A recipe that has not built for both targets is not done.**
Recipe count, throughput, and the 150 budget all count only recipes green for both.

Enforced mechanically: **a recipe file may not contain an architecture triple.**
Anything that differs per target is declared in the target file — kernel config fragment, image contents, `configure` host — and reaches the recipe as a `${target.*}` substitution.
A lint greps every recipe for a triple and fails the build. It is crude, and it is exactly criterion 3.

### The engine is one binary

`kb`, in Rust, no external crates.
Subcommands: `build`, `image`, `boot`, `lint`, `check-provenance`, `report`.

One binary because the constraint the whole spec serves is that a person can read the core in a day, and that includes the tool that builds it.

Three consequences worth stating, because each is a deletion:

- **No sandbox code.** Builds already run inside the seed container; isolation is podman's job and we do not reimplement it.
- **No TOML crate.** A strict parser for the subset we use, which rejects everything outside it — the parser is how the readable subset stays readable.
- **No hash crate.** `sha256sum` from the seed.

### The graph distinguishes build edges from runtime edges

Attempt one's planner broke a cycle by cutting a *runtime* edge, on the assumption the chroot already had it.
Over a minimal seed it did not, and the failure surfaced far from its cause.

`[deps] build` and `[deps] runtime` are separate lists.
Cycles may be broken only across build edges, and cutting a runtime edge is a hard error naming both endpoints.

### Formats, so recipes can be written against them today

```toml
# recipes/zlib.toml
name = "zlib"
version = "1.3.1"

[source]
url = "https://zlib.net/zlib-1.3.1.tar.gz"
sha256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"
license = "Zlib"                # SPDX; required wherever there is a [source]

[build]
system = "configure"          # configure | meson | cmake | make | kernel | shell
args = ["--shared"]

[deps]
build = ["gcc", "binutils"]
runtime = []

[check]                         # DESIGN.md C9: run by the image's selftest, on every target
run = ["test -e /usr/lib/libz.so.1"]
```

```toml
# targets/x86_64-cloud.toml
name = "x86_64-cloud"
triple = "x86_64-koompi-linux-gnu"
arch = "x86_64"
kernel_config = ["core", "hardening", "virt", "x86_64-cloud"]   # DESIGN.md C4, merged in order
firmware = "ovmf"
contents = ["glibc", "linux", "dinit", "bash", "coreutils", "util-linux", "systemd-boot"]

# DESIGN.md C5: policy that names an architecture lives here, never in a recipe
cflags = ["-D_FORTIFY_SOURCE=3", "-fstack-clash-protection", "-fcf-protection=full"]
ldflags = ["-Wl,-z,relro,-z,now"]
setuid = []                     # DESIGN.md C6: the gate fails on anything not listed
```

`system = "shell"` is the escape hatch from spec decision 2.
Its use is counted in the Gate 0 report: if most recipes need it, the format is wrong and we should know that in week one.

### Before Gate 0, because retrofitting costs more than doing

[docs/DESIGN.md](docs/DESIGN.md) makes eleven calls about the running system.
Most wait for the core to boot.
These five are cheap now and expensive at 150 recipes, so they land inside M2 and M3 rather than after:

| Call | What lands | Where |
|---|---|---|
| C10 determinism | the engine exports a fixed `SOURCE_DATE_EPOCH`, `TZ=UTC`, `LC_ALL=C.UTF-8`, `umask 022` and `-ffile-prefix-map=$SRC=/src` in every build script header | M2, engine |
| C5 hardening | `cflags` and `ldflags` in the target file, exported before recipe env; `check-provenance` gains PIE, RELRO, BIND_NOW, NX and no-RWX checks | M2 target file, M3 gate |
| C6 zero setuid | util-linux gets `--disable-makeinstall-setuid`; the image gate fails on any setuid or setgid file outside the target's allowlist, which is empty | M2 recipe, M3 gate |
| licence | `license = "<SPDX>"` required by lint wherever there is a `[source]`; recipes with no source declare the project's licence once Rithy picks one | M2, lint |
| C9 checks | an optional `[check]` list per recipe, concatenated by `kb image` into the selftest that boot-services already runs | M2, engine |

One M2 finding from writing the design: the selftest script calls `grep`, and `grep` is in no target's contents.
Either bash's own `[[ ]]` and `read` replace the two calls, or `grep` becomes the fourteenth recipe; the former is smaller.

## Attempt one's four open blockers, under this shape

All four were ordinary and individually solvable, and all four are structurally absent here rather than fixed:

| Blocker | Why it does not recur |
|---|---|
| no `elfutils`, so `objtool` could not build | `objtool` is a *host* tool; `elfutils` is in the seed, and the kernel is cross-built |
| planner cut a runtime edge | build and runtime edges are separate, and cutting a runtime edge is a hard error |
| `strptime` behind `__USE_XOPEN`, musl vs glibc | glibc from the first build, decided in the spec, no flag day |
| meson recipes could fetch undeclared subprojects | no network in the build container, and every source digest-pinned |

This is the argument for starting again rather than continuing: the failures were shape, not effort.

## Milestones

Day 1 is 2026-09-02. Days are working days.

### M1 — one recipe set, two cross toolchains (D1–D5)

`kb build` end to end: parse, graph, fetch, build in the seed container, content-addressed store.
Recipes: `binutils`, `linux-headers`, `gcc-bootstrap`, `glibc`, `gcc`.

That is five, where this plan first said four with gcc staged internally.
It cannot be four: glibc has to be built by a C compiler, and the full compiler has to be built against glibc, so glibc sits *between* the two gcc passes and each pass is a node in the graph.
Correcting it here rather than quietly building five.

**Exit:** `kb build gcc --target x86_64-cloud` and `--target aarch64-headless` are both green from an empty store, from one recipe set, with `kb lint` clean.
Criterion 3 is proven at the toolchain, where it is hardest, on day five.

**Done, D1.** Both toolchains build and pass `crates/kb/tests/toolchain.sh`, which runs inside the seed container and checks that the compiler knows its target, uses *our* assembler, links C and C++ against our glibc statically and dynamically, emits the right ELF machine, and asks for the loader glibc installed.

Five recipes, two targets, zero forked recipes, `kb lint` clean.
A full toolchain from an empty store is about 17 minutes per target on 8 cores: binutils 53s, gcc-bootstrap 323s, linux-headers 8s, glibc 134s, gcc 480s.

Criterion 3 held, and the value of proving it here rather than at the image is that it did not hold at first.
Three engine defects and one recipe defect only became visible on the second architecture, and one of them — gcc silently using the seed's assembler — was *wrong on x86_64 too* and produced a passing build.
Had aarch64 come last, all four would have been found on top of a working image instead of a bare toolchain.

### M2 — the boot floor, direct kernel boot (D6–D12)

The runtime set: `linux`, `dinit`, `bash`, `coreutils`, `util-linux`, plus what their closure drags in.
`kb image` assembles a rootfs; `kb boot` runs QEMU with `-kernel`, deliberately bypassing the boot chain so a userland failure cannot be confused with a firmware one.

**Exit:** `kb boot x86_64-cloud` reaches a shell prompt. `kb check-provenance` clean.

Known before starting, from M1: gcc bakes a per-architecture dynamic-linker path into every binary — `/lib64/ld-linux-x86-64.so.2` on `x86_64`, `/lib/ld-linux-aarch64.so.1` on `aarch64` — while glibc installs the loader under `/usr/lib`.
Overriding that in the gcc recipe would mean naming an architecture in a recipe, which criterion 3 forbids, so the image carries `/lib` and `/lib64` as symlinks to `usr/lib`.
That needs a `filesystem` recipe with no upstream source, which the recipe format does not yet allow.

The same fact makes one kind of test worthless: on an `x86_64` host, `/lib64/ld-linux-x86-64.so.2` is the *host's* loader, so a binary we cross-built will run there against the host's glibc and look fine.
Only QEMU counts.

### M3 — the boot chain (D13–D17)

UKI, `systemd-boot` built alone from the systemd tree, the ESP write protocol from the post-mortem — hidden staging name, fsync, byte-compare read-back, rename, fsync the directory, retain 3, never collect before a good boot.
`bless-boot` as a dinit oneshot.

This is also where the root stops being an initramfs and becomes the production shape from DESIGN.md:

- **C3**: `kb image` writes the root as erofs with a fixed timestamp, and `veritysetup` produces the hash tree; both tools are added to the seed, with `sbsign` for the UKI
- **C2**: the UKI's command line carries `dm-mod.create=` with the root hash, and the kernel opens the verity root itself; no initramfs, no `/init` before dinit
- **C4**: `config/kernel/` becomes `core`, `hardening`, `virt` and per-target fragments, listed in the target file and merged in order; the "every line survives `olddefconfig`" check runs over all of them
- **C1**: `early-fs` mounts the `/etc` overlay and the `/var` partition; the state partition is created on first boot

**Exit:** `x86_64-cloud` boots under OVMF from the ESP into a verity root, and an entry rigged to fail rolls back unattended across three tries.
Rollback is on the [cut list](#cut-list); UEFI boot and the verity root are not.

### M4 — `aarch64` headless (D18–D20)

Its own target file, its own kernel config fragment, no new recipes beyond what the target genuinely adds.

**Exit:** `kb boot aarch64-headless` reaches a shell. Zero recipes changed to make it work — verified by `git log` over `recipes/`, not by assertion.

### M5 — the Gate 0 report (D21)

`kb report` emits: recipe count against the 150 budget, recipes green for both targets, `shell` escape-hatch count, wall-clock per recipe, and image sizes.

**Exit:** a go/no-go written down against the four criteria, including the option to say no.

## Deterministic gates

Hooks, not habits. Every one fails the build rather than warning:

- `kb lint` — recipe schema, and no architecture triple in any recipe
- every source digest-pinned; an unpinned source will not fetch
- `kb check-provenance` on every image
- **build the commit, not the working tree**: `git archive HEAD | tar -x -C "$(mktemp -d)"`, then build there — the post-mortem's gitignore trap cost a commit that did not build while every test passed
- `cargo test` and `cargo clippy -D warnings` on `kb`

CI is split by cost.
GitHub Actions runs the cheap gates on every push — lint, schema, `cargo test`, the `git archive` build.
Full builds run on the build machine, because a 4-core hosted runner cannot cross-build gcc twice.

## Metrics

The SDLC playbook's numbers, instantiated:

- **Recipes per day**, recorded from D1, counting only recipes green for both targets.
- **First-pass gate success** — the fraction of recipes that pass every gate without a follow-up commit. This is the real readout on whether recipe-writing is mechanical; the throughput bet is won there or not at all.
- **Escape-hatch rate** — `system = "shell"` as a share of recipes. Rising means the format is wrong.
- **Intent survival** — spec decisions reopened. Target zero; attempts two and three both died of decisions made after building.

Attempt one's baseline is a *count*, 214, not a rate; its calendar span went with the deleted repo.
So the comparison Gate 0 can honestly make is against a deadline rather than a rate, and the rate is recorded from D1 so the next decision has one.

## Cut list

Named now, so cutting is a decision and not a quiet slip:

1. unattended rollback in M3 — the ESP write protocol stays, boot counting can wait
2. `zstd`-compressed images — uncompressed boots the same
3. `kb report` as a subcommand — a script produces the same numbers

## After Gate 0: the desktop is target three, and the product

Not dated, because dating it before Gate 0 is the mistake attempts two and three made.
Shaped, because the budgets are only budgets if what they hold is known, and because Rithy has said who this is for: students, enterprises and government, on their PCs.

**The cloud target is small.** DESIGN.md counts it at 23 recipes: the 13 that exist, then `systemd-boot`, `e2fsprogs`, `dhcpcd`, `iproute2`, `openssh` without OpenSSL, `chrony`, `grep`, `sed`, `findutils`, and one Rust binary, `koompi`.
So Gate 0 measures a rate and proves molding; it does not test throughput at the scale that killed attempt one.

**The desktop is where the bet is decided**, and it is also what ships, so it comes straight after Gate 0 in this order, each step gated by a boot on a reference machine rather than in QEMU:

1. **Hardware boot** — modules, `kmod`, firmware, microcode, the small initramfs, LUKS on the state tier (D1, D2). Gate: a reference laptop boots to a shell from its own disk.
2. **Display** — `libudev` chosen by measurement (D3), Mesa, libinput, Wayland, the font stack. Gate: a bare compositor draws on three reference machines.
3. **Session** — KOOMPI Desktop, Qt 6, PAM, the greeter, Flatpak, Khmer fonts, input and the Pango patch (D4, D5, D6). Gate: a student logs in, types Khmer, opens a browser from Flathub.
4. **Ship** — the installer target, `koompi update` end to end with rollback, policy overlay and channels (D7, D9). Gate: install from USB, update, break the update, roll back, on all three machines.
5. **Editions** — `student`, `enterprise`, `government` as compositions (D10). Gate: three images from one desktop target, zero forked recipes, by `git log` over `recipes/`.

Step 2 is the throughput test: it is the first time recipes arrive by the dozen, and the M5 numbers, first-pass gate success and escape-hatch rate, are what predict whether it clears.
The desktop layer's budget is **300 recipes above the core**, held the way the 150 is.

## Not in this plan

Self-hosting · the desktop · signing, keys and release infrastructure · reproducibility as a merge gate · any package manager on the device · targets three and beyond.
The mutable-state story is no longer open: it is DESIGN.md C1, and it lands in M3.

## How this plan dies

**If M2 has not booted by D14, the throughput bet has not cleared, and this stops.**

Not "slips" — stops, and gets written down, as intent.md requires.
M2 is a minimal userland on one architecture with the toolchain already standing.
If that takes more than three weeks with agents, then agent-driven throughput is not an order of magnitude better than hand-written, the premise in intent.md is false, and the honest move is to say so in week three rather than month six.

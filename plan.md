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

[build]
system = "configure"          # configure | meson | cmake | make | kernel | shell
args = ["--shared"]

[deps]
build = ["gcc", "binutils"]
runtime = []
```

```toml
# targets/x86_64-cloud.toml
name = "x86_64-cloud"
triple = "x86_64-koompi-linux-gnu"
arch = "x86_64"
kernel_config = "config/kernel/x86_64-cloud.config"
firmware = "ovmf"
contents = ["glibc", "linux", "dinit", "bash", "coreutils", "util-linux", "systemd-boot"]
```

`system = "shell"` is the escape hatch from spec decision 2.
Its use is counted in the Gate 0 report: if most recipes need it, the format is wrong and we should know that in week one.

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

**Exit:** `x86_64-cloud` boots under OVMF from the ESP, and an entry rigged to fail rolls back unattended across three tries.
Rollback is on the [cut list](#cut-list); UEFI boot is not.

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

## Not in this plan

Self-hosting · the desktop · signing, keys and release infrastructure · reproducibility as a merge gate · any package manager on the device · any mutable-state story · targets three and beyond.

## How this plan dies

**If M2 has not booted by D14, the throughput bet has not cleared, and this stops.**

Not "slips" — stops, and gets written down, as intent.md requires.
M2 is a minimal userland on one architecture with the toolchain already standing.
If that takes more than three weeks with agents, then agent-driven throughput is not an order of magnitude better than hand-written, the premise in intent.md is false, and the honest move is to say so in week three rather than month six.

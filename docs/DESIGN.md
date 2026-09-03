# Design: what a Linux built from scratch in 2026 is

Written 2026-09-03, day two of the fourth attempt, with M1 done and M2 under way.
[intent.md](../intent.md) says why, [spec.md](../spec.md) holds the eight settled decisions, [plan.md](../plan.md) sequences Gate 0.
This file answers a different question: if you were designing a Linux today, with no distribution's habits to inherit, what would it be?

It reopens none of the eight decisions.
Every call below is downstream of them, and each one is marked as either an engineering call made here or a decision that belongs to Rithy.
The calls that cost nothing now and a great deal later are folded into `plan.md` before Gate 0; the rest wait until the core boots.

## The system, in one paragraph

A signed kernel image boots straight into a read-only, verity-protected root, with no initramfs.
PID 1 is dinit.
`/usr` is the image and never changes underneath you.
`/etc` is the image's defaults with your changes layered on top, so "what did I change on this machine" is a directory listing.
`/var` is the only persistent state, on its own filesystem, and it is all a factory reset has to wipe.
Updates are a new image and a new kernel, written beside the old, and made current by a rename.
A bad update rolls back by itself because the boot manager counts tries in the filename.
Nothing in the core needs a daemon to be told about hardware, to log, to authenticate, or to talk on a bus.
The whole thing is small enough to read in a day, and every claim above is a gate the build fails rather than a sentence in a document.

## What the era settled, and this core takes for granted

These are not choices any more.
A distribution that lacks them in 2026 is late, not opinionated.

- **Immutable root, image-based updates, A/B with automatic rollback.** Spec decision 5. What cloud, phone, automotive and appliance targets all already do.
- **UEFI, a Unified Kernel Image, one boot manager.** The post-mortem's boot findings, and spec's "systemd-boot stays".
- **Merged `/usr`, and `/bin`, `/lib`, `/lib64`, `/sbin` as symlinks.** Already in the `filesystem` recipe.
- **cgroup v2 only, user namespaces on, overlayfs, seccomp, Landlock.** Containers are how software reaches a cloud host now, and the core owes them the kernel features and nothing more.
- **64-bit only.** No 32-bit compatibility in the kernel, no multilib in the toolchain. A target that needs it, and a desktop running old games might, adds it to its own kernel fragment.
- **Every binary PIE, RELRO, BIND_NOW, NX, with a stack protector.** The toolchain already defaults to PIE and SSP; the rest is flags, and a gate that reads the ELF back.
- **Reproducible by construction.** Spec says it is a goal and not yet a gate; the engine can still be deterministic from today, because retrofitting determinism is retrofitting cross-compilation all over again.
- **Signed everything, with one key hierarchy.** UKI for Secure Boot, kernel modules, images. Custody is Rithy's; the design has to say where each key is used so there is a place for it when it exists.
- **A bill of materials for every image**, shipped inside the image, with licences. Nobody ships a cloud image without one in 2026, and the licence is the one field that is trivial at 13 recipes and painful at 150.

## The calls

Numbered so `spec.md` and `plan.md` can refer to them.

### C1. Three tiers of state, and nothing else

| Tier | Path | Backing | Lifetime |
|---|---|---|---|
| Image | `/usr` and the root | erofs on dm-verity, read-only | replaced whole by an update |
| Configuration | `/etc` | overlayfs: image defaults below, a directory under `/var` above | survives updates, diffable, wipeable |
| State | `/var`, `/home` | ext4 on the state partition | the machine's own, never touched by an update |

Everything else, `/run`, `/tmp`, `/dev/shm`, is tmpfs and gone at reboot.

The overlay is one mount in `early-fs`, which dinit already runs.
"Factory reset" is deleting one directory under `/var`.
"What did this operator change" is listing it.
No `/usr/etc` split, no `tmpfiles.d`, no `sysusers.d`: the image ships a complete `/etc`, and the overlay is the whole mechanism.

**Engineering call.** The spec left "the on-device story for anything mutable" open; this is the answer.

### C2. No initramfs on virtual targets

The kernel opens the verity device itself from its command line (`dm-mod.create=`, `CONFIG_DM_INIT`), with the root hash in that command line, and the command line baked into the UKI.
The UKI's signature is therefore what authenticates the root hash.
There is no separate root-hash signature, no `veritysetup` on the device, no `cryptsetup`, no `libdevmapper`, and no `/init` that runs before PID 1.

Drivers a virtual target needs are built in, which the kernel fragments already do.
A hardware target that needs LUKS, a TPM, or modules before root will need an initramfs, and that is a target-level cost paid by that target.

The `PARTUUID=` form of the data and hash devices works from Linux 6.5, and the kernel is 6.18.

**Engineering call.** The single largest deletion in the boot path, and it makes M2's "direct kernel boot" the production shape rather than a test shortcut.

### C3. The root is erofs, the state is ext4

erofs is the read-only filesystem designed for exactly this: compressed, verity-friendly, reproducible with a fixed timestamp, and what Android and container runtimes converged on.
`mkfs.erofs` and `veritysetup` run in the seed at image time and never reach the device.
The state partition is ext4 because it is boring, and `e2fsprogs` is one recipe.
The first boot grows the state partition into free space with `sfdisk` from util-linux and creates it with `mkfs.ext4`.

**Engineering call.** btrfs for `/var` would give snapshots of state, at the cost of a much larger userland; the state tier is small and cheap to back up by copying, so it is not worth it in the core.

### C4. Kernel config is shared fragments, and hardening is one of them

Today each target has one 35-line fragment, and the two differ in three lines.
As soon as verity, erofs, cgroups, namespaces and hardening arrive that will be sixty lines duplicated twice, then drifting.

So `config/kernel/` becomes fragments merged in order: `core` (what every KOOMPI kernel is: MODVERSIONS, no debug info, cgroup v2, namespaces, overlayfs, erofs, dm-verity, dm-init, seccomp, Landlock, lockdown), `hardening` (stack protector strong, KASLR, `init_on_alloc`, hardened usercopy, `FORTIFY_SOURCE`, randomised and hardened freelists, zero call-used registers, no `/dev/mem`, no `kcore`, no legacy `TIOCSTI`, no vsyscall, no 32-bit emulation), `virt` (virtio), then the target's own.
A target file lists its fragments; the existing "every fragment line survives `olddefconfig`" check covers all of them.

Module signing is in `core` with a build-time key until custody is settled, and `MODULE_SIG_FORCE` turns on the day the real key exists.
The AI appliance target is why this matters: a proprietary driver is an out-of-tree module signed with our key against our `MODVERSIONS` kernel, and the post-mortem measured what happens when that is retrofitted.

**Engineering call.**

### C5. Hardening flags live in the target file, and a gate reads the binaries back

The architecture-neutral set, `-D_FORTIFY_SOURCE=3 -fstack-clash-protection -Wl,-z,relro,-z,now`, and the architecture-specific one, `-fcf-protection=full` on x86_64, `-mbranch-protection=standard` on aarch64, are both policy.
A recipe may not name an architecture, so they belong in the target file as `cflags` and `ldflags`, and the engine exports them for target recipes, before recipe environment so a recipe can still override.

`kb check-provenance` already walks every ELF in an image.
It grows a hardening pass: PIE, `PT_GNU_RELRO`, `BIND_NOW`, non-executable stack, no writable-and-executable segment, and the CET or BTI property note on the architecture that has one.
Canaries and fortification are compile-time policy, verified by the flags being in the target file, not by reading symbols.

**Engineering call.**

### C6. Zero setuid binaries, by default and by gate

The core has no non-root user who needs `su`, `mount` or `passwd`.
util-linux installs `mount` setuid unless told not to; the recipe is told not to.
The image gate fails on any setuid or setgid file not listed in the target's allowlist, and the allowlist is empty for both Gate 0 targets.
A desktop target that needs one names it, with a reason, in its own target file.

**Engineering call.**

### C7. No udev, no PAM, no D-Bus, no TLS library in the core

Each is a subsystem that, once in, never comes out, and none is needed to boot two targets and run services on them.

- **Devices.** devtmpfs creates the nodes. Drivers are built in on virtual targets. A hardware target coldplugs by walking `/sys` for `modalias` and calling `modprobe`, which is a dozen lines of shell. `libudev`, which every display stack demands, enters with the desktop layer and not before.
- **Authentication.** Cloud is SSH keys only, root locked, no password anywhere, so there is nothing for PAM to mediate. `login` from util-linux builds without it. PAM is a desktop-layer decision.
- **Bus.** dinit talks over its own socket. D-Bus arrives with the first desktop component that needs it.
- **TLS.** OpenSSH builds without OpenSSL and still speaks the modern set: ed25519, curve25519, chacha20-poly1305. Updates are signed images fetched over plain HTTP, integrity from the signature and not the transport, which is what every package manager already relied on before HTTPS was universal. A Rust tool with no crates cannot speak TLS, and the honest response is to design so it does not have to.

**Engineering call, with one part for Rithy:** whether an image download that anyone can observe is acceptable for every target, or whether the AI appliance and phone need transport privacy and therefore a TLS library in a layer above.

### C8. The KOOMPI tools are one Rust binary on the device

`koompi` with subcommands: `firstboot` (hostname, SSH keys and partition growth from NoCloud or the instance metadata service), `update` (fetch, verify, write the inactive slot, stage the UKI with the post-mortem's ESP protocol), `bless` (the rename after a good boot), and `state` (list and reset the configuration overlay).
No crates, so it carries its own HTTP/1.1 client, SHA-512 and ed25519 verification, which together are a few hundred readable lines and a well-known test vector set.

Until self-hosting, it is compiled by the seed's rustc against a prebuilt standard library.
That library is the one borrowed artifact that reaches an image, and `check-provenance` should list it as such rather than fail or ignore it.

**Engineering call**, and the same spirit as `kb` being one binary.

### C9. A recipe declares how it is checked, and the image runs those checks

The `selftest` service already runs on every boot and speaks only when asked.
A recipe gains an optional `[check]` with one or more commands; `kb image` concatenates them into the selftest.
"Green for both targets" then means a recipe's own check ran under QEMU on both, not that it compiled twice.
This is the mechanical version of "verified by booting, not by building" from intent.md, applied per recipe, and it is what makes an agent's recipe self-grading.

**Engineering call.**

### C10. Determinism is in the engine now, and a gate later

The engine already emits sorted, zero-mtime cpio and configures binutils with deterministic archives.
It does not yet set `SOURCE_DATE_EPOCH`, `TZ`, `LC_ALL`, `umask`, or `-ffile-prefix-map`.
Those are five lines in the script header the engine writes, and every recipe built after they land is one step from bit-identical.
The epoch is one constant, project-wide, so no recipe carries a date.

Bit-identical remains a goal until Gate 0 clears, as the spec says; this only stops making it harder.

**Engineering call.**

### C11. Budgets, not estimates, for size and time

The 150-recipe budget works because it is a number that fails a build.
Two more, in the same spirit, proposed here and set at M5 once measured:

- the `x86_64` cloud root image, uncompressed, under **128 MiB**
- kernel entry to a shell prompt under QEMU, **under 2 seconds**, no initramfs, monolithic kernel

The second is what automotive and phone will ask first, and the shape that makes it possible, C2 and C4, is decided in the core and not in those targets.

**Rithy's call on the numbers; engineering call that there are numbers.**

## The `x86_64` cloud target, counted

What a cloud image is in 2026, in recipes, against the 150 budget:

| Layer | Recipes | Count |
|---|---|---|
| Floor, exists today | filesystem, boot-services, linux, linux-headers, binutils, gcc-bootstrap, gcc, glibc, gcc-libs, dinit, bash, coreutils, util-linux | 13 |
| Boot chain | systemd-boot | 1 |
| State | e2fsprogs | 1 |
| Network | dhcpcd, iproute2 | 2 |
| Access | openssh, without OpenSSL | 1 |
| Time | chrony | 1 |
| Scripts stand on | grep, sed, findutils | 3 |
| KOOMPI | koompi | 1 |
| | | **23** |

The build-only tools, `mkfs.erofs`, `veritysetup`, `sbsign`, meson and Python for systemd-boot, live in the seed and cost nothing here.
Not in the image at all: a container runtime, a firewall, an editor, a syslog, a package manager, certificates.
The first two are the cloud layer's, composed onto this core; the rest have no job on a host whose only shell arrives over SSH.

Twenty-three is a sixth of the budget.
That is worth stating plainly because of what it means for the throughput bet.

## What Gate 0 does and does not prove

Gate 0 proves the molding claim, the readability claim, and the boot.
It measures a recipe rate.
It does not test throughput at the scale that killed attempt one, because a cloud core needs a few dozen recipes and attempt one stalled at two hundred.

The place the throughput bet is decided is the desktop layer, where the post-mortem measured 354 packages closed over dependencies.
That is not an argument against Gate 0.
It is the reason the Gate 0 report has to record first-pass gate success and the escape-hatch rate honestly, because those two numbers, and not the count, are what predict whether 354 is reachable.

## The product: PCs for students, enterprise and government

Rithy, 2026-09-03: cloud is not the real use.
The real use is general users, students, enterprises and government, running this on their PCs.

That answers the first open item in intent.md, and it changes what the design above is *for* without changing any of it.
The cloud and headless targets stay as Gate 0's proof of molding, because they are cheap, twenty-three recipes, and dissimilar.
The desktop is target three, the first one that ships to a person, and the place the throughput bet is actually decided.

### What each audience needs, and what in the core already answers it

| Audience | Needs | Answered by |
|---|---|---|
| Students | cheap laptops, low memory, poor links, shared lab PCs, Khmer everywhere | small image and fast boot (C11); reset-on-logout is one directory delete (C1); offline and peer-cached updates (D7); fonts, input and the line-break patch (D5) |
| Enterprise | fleets, enforced configuration, directory login, disk encryption, staged updates | policy as a lower `/etc` layer (D7); LUKS on the state tier (D2); A/B with rollback (decision 5); channels and pinning (D7) |
| Government | sovereignty: built from source, signed with keys held at home, auditable, no telemetry, works offline | the core itself: from source, readable, reproducible (C10), a bill of materials in every image, keys in C4 and C8, nothing phones home (D8) |

The last row is the pitch.
A government cannot audit Windows, and cannot in practice audit a respin either, because a respin's bill of materials is somebody else's.
A core one person reads in a day, built from digest-pinned sources with a shipped manifest, is a claim a ministry's own engineer can check.

### The desktop calls

Numbered on from the core's, and all of them land after Gate 0.

**D1. A hardware kernel, with modules.**
The desktop's kernel fragments are `core`, `hardening`, `hardware` and its own.
Modules are on, because the hardware is unknown until boot; `kmod`, a firmware subset and CPU microcode join the image.
`MODVERSIONS` and module signing from C4 are what make this safe to have been decided already.

**D2. A small initramfs, and only the state tier is encrypted.**
The root is public: it is the same verity image on every machine, and encrypting it would protect nothing.
`/var` and `/home` are LUKS2, and that is where every private byte lives.
So the desktop needs what C2 let virtual targets skip, an initramfs, but a small one: `kmod`, `cryptsetup`, and `koompi init`, which loads modules by modalias, opens the state volume, and hands the verity root to dinit.
Passphrase first; TPM2 unlock later, spoken to `/dev/tpmrm0` from the `koompi` binary rather than through `tpm2-tss`.

**D3. `libudev` arrives, and the implementation is chosen by measurement.**
Mesa, libinput and every compositor want the `libudev` API.
The candidates are `eudev`, a fork of systemd's, and `libudev-zero` over a coldplug script, a few thousand lines.
The criterion is three reference laptops running the KOOMPI Desktop session, and the smaller one wins if it passes.

**D4. Wayland only, KOOMPI Desktop as the session.**
No X server; Xwayland for the applications that still need it.
The session is the Hyprland and Quickshell desktop already developed in `koompi-desktop`, which brings Qt 6 and its closure into the layer.
That closure, Mesa, the font stack, Wayland, libinput, Qt, is the bulk of the desktop budget and is where recipe throughput gets measured at the scale that killed attempt one.

**D5. Applications are Flatpaks; the core and the desktop are from source.**
A browser and an office suite cannot be built from source inside any readable budget, and every immutable desktop of this era reached the same answer.
Flathub is upstream-published and signed, so this is not downstream of a distribution.
An enterprise or a school mirrors the remote for its own network, which is also how it stays offline and how it decides what its users may install.
Khmer is the exception that is ours: the fonts, the input method, and the Pango line-break patch from the post-mortem, carried in the recipe and sent upstream.

**D6. PAM enters here, and directory login is an edition cost.**
Login, lock and the greeter need it, and the desktop is where C7 said it might arrive.
Local accounts first.
Directory login through `sssd` is large, and belongs to the enterprise edition, not the desktop target; a smaller path is an open question.

**D7. Management is the `/etc` overlay with one more layer.**
Image defaults below, organisation policy in the middle, the user's changes on top.
Policy arrives as a signed archive through `koompi update`, from a channel the organisation picks: `stable`, `edu`, `gov`, or its own.
Updates can be pinned, staged by fraction of a fleet, applied from a USB stick, and fetched from a peer on the LAN before the internet, because a school with forty PCs has one slow link.
Inventory, the bill of materials plus hardware and version, is a `koompi report` that the organisation may point at its own endpoint; off by default, and never on for a student.

**D8. Nothing phones home.**
No telemetry, no crash upload, no update check that carries an identifier.
The update fetch is the one network call the core makes, and it sends nothing but the channel name.

**D9. An installer is a target, not a tool.**
`x86_64-installer` is the desktop image booted from USB with `koompi install` as its session, which writes the ESP, the A/B slots and the encrypted state tier.
That is the molding claim applied to the product: the installer is a composition, and it shares every recipe.

**D10. Editions are compositions.**
`student`, `enterprise` and `government` are target files that extend `x86_64-desktop` with contents, policy and channel.
Attempt two got this right and the rule stands: editions are never forks.

**D11. Hardware support is a named list.**
Three reference machines, the ones KOOMPI sells or the ones the audiences actually own, and a boot on each is the gate; "works on most laptops" is not a status.

### The desktop budget

The core's 150 holds and is not the desktop's number.
The post-mortem measured a desktop base at 354 packages closed over dependencies, on a distribution with far more in it than this.
The desktop layer is budgeted at **300 recipes above the core**, and its readability claim is a week rather than a day.
The Flatpak decision in D5 is what makes that number possible: without it, one browser costs more than the whole layer.

### What this changes above

- intent.md's first open item is answered, and its "not desktop-first" line now means sequencing, not product.
- plan.md's "after Gate 0" section names the desktop as target three, the throughput test, and the first thing that ships.
- C7's TLS question is settled for the desktop: Flatpak brings TLS with it, so the desktop layer has one, and the core still does not.
- C11's boot-time budget matters more, not less: a student's laptop is where two seconds is felt.

## Deliberately absent, and where each goes

| Absent from the core | Because | Where it lives |
|---|---|---|
| systemd | decision 4 | nowhere |
| udev, libudev | C7 | desktop layer, D3 |
| PAM | C7 | desktop layer, D6 |
| D-Bus, polkit | C7 | desktop layer |
| OpenSSL, certificates | C7 | desktop layer, with Flatpak (D5) |
| initramfs | C2 | desktop target, small (D2) |
| Python, Perl on the device | build tools only | seed |
| 32-bit compatibility | 64-bit only | a target's own kernel fragment |
| a syslog | dinit logs per service; the serial console is the log on cloud | a layer that wants aggregation |
| a container runtime | the core owes containers the kernel, not the runtime | cloud layer |
| Khmer fonts, input, and the Pango line-break patch | no display stack in the core | desktop layer, D5; the patch goes upstream |

## Open, and Rithy's

- Whether image downloads without transport privacy are acceptable for every target (C7); the desktop has TLS through Flatpak regardless.
- The two numbers in C11.
- Who may get a shell from a cloud target's serial console. Today it is anyone, because `agetty --skip-login` is the boot floor's console; before Gate 0 ships an image it has to be a target-file choice, defaulting to off for cloud.
- The project's own licence, which is what the `filesystem`, `boot-services`, `gcc-libs` and `koompi` recipes would declare.
- Signing key custody, already open in intent.md; C4 and C8 are where the keys are used.
- Secure Boot on PCs that ship with Microsoft's keys: enrol KOOMPI's key on the reference machines, or submit a shim for Microsoft signing. The first is fine for fleets and wrong for a student's own laptop.
- The three reference machines (D11).
- Whether directory login is worth `sssd`, or waits for something smaller (D6).

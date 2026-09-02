# Intent: KOOMPI Linux

Stage 1 of the agent-native SDLC. This file is the *why* and the *what*.
No design decisions belong here — those go in [spec.md](spec.md).

Status: **Build.** [spec.md](spec.md) settled the design; [plan.md](plan.md) sequences the work to Gate 0.

## What this is

An original Linux distribution.
Its own userland, built from source, downstream of no existing distribution.

## The claim

One core that molds into any target: cloud, desktop, phone, automotive, AI appliance.

The molding is the product.
A core that only produces desktops is not this, and would not have been worth starting.

## Why not a respin

Three previous attempts are recorded in [docs/POSTMORTEM.md](docs/POSTMORTEM.md), and the fourth exists because of what they measured.
The first built its own toolchain and booted to a login prompt before stalling on throughput — 214 recipes, written by hand.
The second and third were respins at different layers, and each was abandoned within days for a reason that should have been settled before either began.

The bet this attempt makes is narrow and testable: **agent-driven recipe throughput is an order of magnitude better than hand-written**, and that is exactly the constraint that killed the first attempt.
If that bet is wrong, this ends the same way, and the point of starting with a falsification gate is to find out in week two.

## Influences, not bases

**Arch** — recipes that are readable shell rather than a framework, a rolling base, and a system whose owner can see all of it.

**Alpine** — smallness as a design constraint rather than an outcome, and a base a person can hold in their head.

These two pull in opposite directions on libc, init and package format.
That conflict is real, it is the first thing `spec.md` has to settle, and it gets settled once.

## What "moldable" has to mean, testably

Not a slogan. The requirement is:

- one recipe set and one build graph produce **two dissimilar targets** without forking either
- both targets are verified by booting, not by building
- adding a third target changes composition, never the recipes

Which two targets go first is a spec decision. The requirement that there be two, proven by boot, is not.

## What this is not

- Not a fork, respin, or rebadge of any existing distribution
- Not desktop-first — the desktop starts after the core boots on two dissimilar targets
- Not a package manager wrapped around somebody else's repository
- Not a research project. It has to boot.

## How this intent dies

Written down so that abandoning it is a decision rather than a drift:

1. **Throughput.** If agent-driven recipes do not clear the 214-recipe baseline by roughly an order of magnitude, the premise is wrong and this should stop.
2. **Molding.** If two dissimilar targets cannot come from one graph without forking, the differentiator does not exist and this is a respin wearing a new name.
3. **Duplication.** If what we build is what Yocto, Buildroot or Nix already are, the honest move is to contribute there instead. `spec.md` has to state the difference in a sentence, in week one, not month six.

Any of the three, and the answer is to stop and say so — not to restart at a different layer.

## Open, and owned by Rithy rather than by engineering

- Who this is for, in one sentence
- Whether this is a company product or a personal project
- Signing key custody

None of them block `spec.md`. All of them block shipping.

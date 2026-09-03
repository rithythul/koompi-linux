#!/usr/bin/env bash
# M2's exit criterion, as a command: the image assembles, everything in it is
# ours, and it boots in QEMU far enough to run its own selftest under dinit.
#
# The selftest lives in the image (recipes/filesystem.toml) and runs only
# when the kernel command line asks, so what it exercises is the boot every
# other boot gets. This script only asks, and reads the answer.
#
# Usage: crates/kb/tests/boot.sh <target>
set -euo pipefail

target=${1:?usage: boot.sh <target>}

cargo build -q -p kb
kb="$PWD/target/debug/kb"

"$kb" image "$target"
"$kb" check-provenance "$target"
"$kb" boot "$target" --smoke
echo "boot.sh: $target verified"

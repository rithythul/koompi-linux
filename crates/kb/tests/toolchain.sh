#!/usr/bin/env bash
# Does the built cross toolchain actually produce binaries for its target?
#
# This runs *inside the seed container*, with the same mounts a build gets,
# and that is not a detail. A cross compiler bakes absolute paths into itself:
# its sysroot is /kb/sysroot and its assembler is /kb/store/<id>/bin/..., and
# neither exists on the host. Run these checks outside a container and gcc
# silently falls back to whatever the host has, which is exactly the bug this
# script exists to catch.
#
# It does not run the binaries it builds: that is M2's job, in QEMU.
#
# Usage: crates/kb/tests/toolchain.sh <target>
set -euo pipefail

target=${1:?usage: toolchain.sh <target>}

cargo build -q -p kb
kb="$PWD/target/debug/kb"

triple=$("$kb" targets | awk -v t="$target" '$1 == t { print $2 }')
[ -n "$triple" ] || { echo "no target $target" >&2; exit 1; }

# The ELF machine each architecture should produce, as readelf spells it.
# Explicit rather than derived: a check that computes its answer the same way
# the thing under test does is not a check.
case "${triple%%-*}" in
    x86_64)  want_machine="Advanced Micro Devices X86-64" ;;
    aarch64) want_machine="AArch64" ;;
    *) echo "toolchain.sh does not know the ELF machine for $triple" >&2; exit 1 ;;
esac

gccdir=$("$kb" build gcc --target "$target" | tail -1)
sysroot=$("$kb" sysroot gcc --target "$target")

echo "toolchain: $target ($triple)"
echo "  gcc:     $gccdir"
echo "  sysroot: $sysroot"
echo

# Mounts mirror crates/kb/src/build.rs. If those change, change these.
#
# -i matters: without it podman does not attach stdin, `bash -s` reads an empty
# script, and the container exits 0 having checked nothing. The grep for the
# final marker below exists because that failure looked exactly like a pass.
status=0
output=$(podman run -i --rm --network=none \
    -v "$(dirname "$gccdir"):/kb/store:ro" \
    -v "$sysroot:/kb/sysroot:ro" \
    -e TRIPLE="$triple" \
    -e GCCID="$(basename "$gccdir")" \
    -e WANT_MACHINE="$want_machine" \
    "$(cat seed/DIGEST)" bash -euo pipefail -s <<'INSIDE'
fails=0
check() {
    if [ "$2" = "$3" ]; then echo "  ok   $1"
    else echo "  FAIL $1: expected [$3], got [$2]"; fails=$((fails + 1)); fi
}
ok() {
    if "${@:2}" >/tmp/cmd.log 2>&1; then echo "  ok   $1"
    else echo "  FAIL $1"; sed 's/^/         /' /tmp/cmd.log; fails=$((fails + 1)); fi
}

cc="/kb/store/$GCCID/bin/$TRIPLE-gcc"
cxx="/kb/store/$GCCID/bin/$TRIPLE-g++"
readelf=$(ls /kb/store/*binutils*/bin/"$TRIPLE"-readelf | head -1)

check "the compiler knows its own target" "$("$cc" -dumpmachine)" "$TRIPLE"

# The one that hid on x86_64: gcc's driver looks for an unprefixed `as`, and
# the seed has one. Where the seed's architecture matches the target it works,
# and everything is quietly assembled by the seed's binutils. --with-as bakes
# an absolute path in, and gcc uses it only if that path is executable -- so
# this check is only meaningful in here, where the store is mounted.
check "the assembler is ours, not the seed's" \
    "$(case "$("$cc" -print-prog-name=as)" in /kb/store/*) echo yes ;; *) echo "no: $("$cc" -print-prog-name=as)" ;; esac)" \
    "yes"

cat > /tmp/hello.c <<'C'
#include <stdio.h>
int main(void) { printf("hello from %s\n", "koompi"); return 0; }
C
cat > /tmp/hello.cc <<'CC'
#include <string>
#include <iostream>
int main() { std::string s = "koompi"; std::cout << s << "\n"; return 0; }
CC

# No --sysroot: the one compiled into gcc is /kb/sysroot, and it is mounted.
ok "C compiles and links against our glibc" "$cc" /tmp/hello.c -o /tmp/hello
ok "C links statically" "$cc" -static /tmp/hello.c -o /tmp/hello-static
ok "C++ compiles and links against our libstdc++" "$cxx" /tmp/hello.cc -o /tmp/hello-cc

if [ -f /tmp/hello ]; then
    check "the output is for the target architecture" \
        "$("$readelf" -h /tmp/hello | awk -F: '/Machine:/ { sub(/^ +/, "", $2); print $2 }')" \
        "$WANT_MACHINE"

    # gcc bakes a per-architecture interpreter path -- /lib64/ld-linux-x86-64.so.2
    # on x86_64, /lib/ld-linux-aarch64.so.1 on aarch64 -- while glibc installs
    # the loader under /usr/lib. Overriding that in a recipe would mean naming an
    # architecture in a recipe, which criterion 3 forbids, so the image supplies
    # /lib and /lib64 as symlinks. What the toolchain must get right is the name.
    interp=$("$readelf" -l /tmp/hello |
             sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
    check "the interpreter it asks for is the one glibc installed" \
        "$([ -f "/kb/sysroot/usr/lib/$(basename "$interp")" ] && echo yes || echo "no: $interp")" \
        "yes"

    check "it needs our libc" \
        "$("$readelf" -d /tmp/hello | grep -c 'Shared library: \[libc\.so\.6\]' || true)" "1"
fi

echo
if [ "$fails" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "$fails check(s) failed"; exit 1; fi
INSIDE
) || status=$?

printf '%s\n' "$output"

if [ "$status" -ne 0 ]; then
    echo "toolchain.sh: $target FAILED"
    exit 1
fi
if ! printf '%s' "$output" | grep -q "ALL CHECKS PASSED"; then
    echo "toolchain.sh: the checks did not run; the container exited 0 without reaching the end"
    exit 1
fi
echo "toolchain.sh: $target verified"

#!/usr/bin/env bash
# Does the built cross toolchain actually produce binaries for its target?
#
# M1's exit criterion is a toolchain, and "it compiled" is not the same claim
# as "it produces target code linked against our glibc". This asks the second
# question. It does not run anything: that is M2's job, in QEMU.
#
# Usage: crates/kb/tests/toolchain.sh <target>
set -euo pipefail

target=${1:?usage: toolchain.sh <target>}

cargo build -q -p kb
kb="$PWD/target/debug/kb"

triple=$("$kb" targets | awk -v t="$target" '$1 == t { print $2 }')
[ -n "$triple" ] || { echo "no target $target" >&2; exit 1; }

# The ELF machine each architecture should produce, as readelf spells it.
# Explicit rather than derived: a check that computes the answer the same way
# the thing under test does is not a check.
case "${triple%%-*}" in
    x86_64)  want_machine="Advanced Micro Devices X86-64" ;;
    aarch64) want_machine="AArch64" ;;
    *) echo "toolchain.sh does not know the ELF machine for $triple" >&2; exit 1 ;;
esac

gccdir=$("$kb" build gcc --target "$target" | tail -1)
sysroot=$("$kb" sysroot gcc --target "$target")
cc="$gccdir/bin/$triple-gcc"
cxx="$gccdir/bin/$triple-g++"

# binutils has its own store entry, and only the one for this target has a
# readelf under this triple, so the glob cannot pick the wrong architecture.
readelf=$(ls "$gccdir"/../*binutils*/bin/"$triple"-readelf 2>/dev/null | head -1)
[ -x "$readelf" ] || { echo "no $triple-readelf in the store" >&2; exit 1; }

# gcc's driver finds the cross assembler and linker on PATH, because binutils
# has its own store entry rather than sharing gcc's prefix. The engine puts
# every host-tool dependency on PATH inside the container; this reproduces
# that, and without it the driver fails with "cannot find 'ld'".
export PATH="$(dirname "$readelf"):$PATH"

work=$(mktemp -d "${TMPDIR:-/tmp}/kb-toolchain.XXXXXX")
trap 'rm -rf "$work"' EXIT

fails=0
check() {
    if [ "$2" = "$3" ]; then echo "  ok   $1"
    else echo "  FAIL $1: expected [$3], got [$2]"; fails=$((fails + 1)); fi
}
ok() {
    if "${@:2}" >"$work/cmd.log" 2>&1; then echo "  ok   $1"
    else echo "  FAIL $1"; sed 's/^/         /' "$work/cmd.log"; fails=$((fails + 1)); fi
}

echo "toolchain: $target ($triple)"
echo "  gcc:     $gccdir"
echo "  sysroot: $sysroot"
echo

check "the compiler knows its own target" "$("$cc" -dumpmachine)" "$triple"

# The one that hid: gcc looks for an unprefixed `as` and falls back to PATH,
# where the seed has its own. When the seed's architecture matches the target
# it works, and everything is quietly assembled by the seed's binutils.
as_used=$("$cc" -print-prog-name=as)
check "the assembler is ours, not the seed's" \
    "$(case "$as_used" in "$(dirname "$gccdir")"/*) echo yes ;; *) echo "no: $as_used" ;; esac)" \
    "yes"

cat > "$work/hello.c" <<'C'
#include <stdio.h>
int main(void) { printf("hello from %s\n", "koompi"); return 0; }
C
cat > "$work/hello.cc" <<'CC'
#include <string>
#include <iostream>
int main() { std::string s = "koompi"; std::cout << s << "\n"; return 0; }
CC

# --sysroot on the command line: the one compiled in points at the path the
# build container mounts, which does not exist out here.
ok "C compiles and links against our glibc" \
    "$cc" --sysroot="$sysroot" "$work/hello.c" -o "$work/hello"
ok "C links statically" \
    "$cc" --sysroot="$sysroot" -static "$work/hello.c" -o "$work/hello-static"
ok "C++ compiles and links against our libstdc++" \
    "$cxx" --sysroot="$sysroot" "$work/hello.cc" -o "$work/hello-cc"

if [ -f "$work/hello" ]; then
    check "the output is for the target architecture" \
        "$("$readelf" -h "$work/hello" | awk -F: '/Machine:/ { sub(/^ +/, "", $2); print $2 }')" \
        "$want_machine"
    # gcc bakes a per-architecture interpreter path -- /lib64/ld-linux-x86-64.so.2
    # on x86_64, /lib/ld-linux-aarch64.so.1 on aarch64 -- while glibc installs
    # the loader under /usr/lib. Overriding that in a recipe would mean naming
    # an architecture in a recipe, which is exactly what criterion 3 forbids,
    # so the image supplies /lib and /lib64 as symlinks to usr/lib instead.
    # What the toolchain has to get right is the file name.
    interp=$("$readelf" -l "$work/hello" |
             sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
    check "the interpreter it asks for is the one glibc installed" \
        "$([ -f "$sysroot/usr/lib/$(basename "$interp")" ] && echo yes || echo "no: $interp")" \
        "yes"
    check "it needs our libc" \
        "$("$readelf" -d "$work/hello" | grep -c 'Shared library: \[libc\.so\.6\]' || true)" \
        "1"
fi

echo
if [ "$fails" -eq 0 ]; then
    echo "toolchain.sh: $target verified"
else
    echo "toolchain.sh: $fails check(s) failed"
    exit 1
fi

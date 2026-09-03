#!/usr/bin/env bash
# End-to-end check of the engine against a real container, with a source it
# builds itself. No network: the recipe fetches over file://, which is still
# the fetch-verify-unpack path everything else uses.
#
# `cargo test` covers the parts that are pure logic. This covers the part that
# is a process in a container, which is where the post-mortem says the
# interesting failures live -- an allowlist that refused every package passed
# every unit test and was only caught by running the real binary.
#
# Usage: crates/kb/tests/engine.sh [path-to-kb]
set -euo pipefail

kb=${1:-}
if [ -z "$kb" ]; then
    cargo build -q -p kb
    kb="$PWD/target/debug/kb"
fi
[ -x "$kb" ] || { echo "no kb binary at $kb" >&2; exit 1; }

seed_digest="$PWD/seed/DIGEST"
[ -f "$seed_digest" ] || { echo "no seed/DIGEST; build the seed first" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/kb-engine-test.XXXXXX")
trap 'rm -rf "$work"' EXIT

fails=0
check() {
    if [ "$2" = "$3" ]; then
        echo "  ok   $1"
    else
        echo "  FAIL $1: expected [$3], got [$2]"
        fails=$((fails + 1))
    fi
}
contains() {
    case "$2" in
        *"$3"*) echo "  ok   $1" ;;
        *) echo "  FAIL $1: [$3] not in output:"; printf '%s\n' "$2" | sed 's/^/         /'
           fails=$((fails + 1)) ;;
    esac
}

# A source tree that builds in a second and installs one file.
mkdir -p "$work/upstream/hello-1.0"
cat > "$work/upstream/hello-1.0/Makefile" <<'MK'
all:
	echo "built for $(TRIPLE)" > out.txt

install:
	mkdir -p $(OUT)/usr/bin
	cp out.txt $(OUT)/usr/bin/hello
MK
tar -czf "$work/upstream/hello-1.0.tar.gz" -C "$work/upstream" hello-1.0
sum=$(sha256sum "$work/upstream/hello-1.0.tar.gz" | cut -d' ' -f1)

repo="$work/repo"
mkdir -p "$repo/recipes" "$repo/targets" "$repo/seed" "$repo/config/kernel"
cp "$seed_digest" "$repo/seed/DIGEST"
echo "CONFIG_TEST=y" > "$repo/config/kernel/testtarget.config"
cat > "$repo/targets/testtarget.toml" <<'T'
name = "testtarget"
triple = "x86_64-koompi-linux-gnu"
arch = "x86_64"
kernel_arch = "x86"
kernel_config = "config/kernel/testtarget.config"
contents = ["hello"]

[boot]
machine = "q35"
console = "ttyS0"
T
write_recipe() {
    cat > "$repo/recipes/hello.toml" <<R
name = "hello"
version = "1.0"
kind = "target"

[source]
url = "file://$work/upstream/hello-1.0.tar.gz"
sha256 = "$1"
license = "MIT"

[build]
system = "make"
$2
R
}

cd "$repo"

echo "1. a clean build installs into the store"
write_recipe "$sum" ""
out=$("$kb" build hello --target testtarget 2>&1) || { echo "$out"; exit 1; }
contains "reports the build" "$out" "build hello 1.0 for testtarget"
entry=$(ls -d "$repo"/build/store/*-hello-1.0)
check "the marker is written last" "$([ -f "$entry/.kb-ok" ] && echo yes)" "yes"
check "the install ran" "$(cat "$entry/usr/bin/hello")" "built for x86_64-koompi-linux-gnu"
check "the work dir is cleaned up on success" "$(ls "$repo/build/work" 2>/dev/null | wc -l)" "0"
contains "the build is recorded" "$(cat "$repo/build/builds.tsv")" "hello	1.0	testtarget"
# tar runs as root in the container. Without --no-same-owner it restores the
# uids in the tarball, which land in the host's subuid range and leave a tree
# the invoking user cannot delete.
check "nothing is left owned by a mapped uid" \
    "$(find "$repo/build" -not -user "$(id -u)" | wc -l)" "0"

echo "2. building again uses the store"
out=$("$kb" build hello --target testtarget 2>&1)
contains "reports a cache hit" "$out" "(cached)"

echo "3. a store entry without its marker is rubbish, not a package"
rm "$entry/.kb-ok"
out=$("$kb" build hello --target testtarget 2>&1)
contains "rebuilds rather than trusting it" "$out" "build hello 1.0"
check "and the marker is back" "$([ -f "$entry/.kb-ok" ] && echo yes)" "yes"

echo "4. a source that does not match its recipe is refused"
write_recipe "$(printf 'a%.0s' $(seq 64))" ""
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "names both digests" "$out" "but $repo/recipes/hello.toml pins"
check "and nothing new is built" "$(ls -d "$repo"/build/store/*-hello-1.0 | wc -l)" "1"

echo "5. a failing build leaves no store entry"
write_recipe "$sum" 'make = ["this-target-does-not-exist"]'
before=$(ls "$repo/build/store" | wc -l)
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "shows the real error" "$out" "No rule to make target"
contains "points at the script" "$out" "build.sh"
check "no store entry is left" "$(ls "$repo/build/store" | wc -l)" "$before"
check "the work dir is kept for debugging" "$(ls "$repo/build/work" | wc -l)" "1"
contains "the failure is recorded" "$(tail -1 "$repo/build/builds.tsv")" "fail"

echo "6. a build that installs nothing is a failure, not a success"
# `make all` succeeds and puts its output in the build tree, never in $OUT.
write_recipe "$sum" 'install = ["all"]'
before=$(ls "$repo/build/store" | wc -l)
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "says what went wrong" "$out" "installed nothing into \$OUT"
contains "and points at the likely cause" "$out" "where the command line always wins"
check "no store entry is left" "$(ls "$repo/build/store" | wc -l)" "$before"

echo "7. a recipe that names an architecture is refused"
write_recipe "$sum" 'make = ["--host=x86_64-koompi-linux-gnu"]'
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "the lint fires before the build" "$out" "put it in the target file"

echo "8. a recipe with no upstream builds from its script alone"
cat > "$repo/recipes/layout.toml" <<'R'
name = "layout"
version = "1"
kind = "target"

[build]
system = "shell"
script = """
install -d "$OUT/usr/lib"
ln -s usr/lib "$OUT/lib64"
printf 'config=%s\n' "$(cat "$KERNEL_CONFIG")" > "$OUT/usr/lib/seen"
"""
R
write_recipe "$sum" ""
out=$("$kb" build layout --target testtarget 2>&1) || { echo "$out"; exit 1; }
entry=$(ls -d "$repo"/build/store/*-layout-1)
check "the symlink was installed" "$(readlink "$entry/lib64")" "usr/lib"
check "the kernel config fragment was mounted" "$(cat "$entry/usr/lib/seen")" "config=CONFIG_TEST=y"

echo "9. editing the kernel config fragment rebuilds only what read it"
"$kb" build hello --target testtarget >/dev/null 2>&1
echo "CONFIG_TEST=n" > "$repo/config/kernel/testtarget.config"
out=$("$kb" build hello --target testtarget 2>&1)
contains "a recipe that never read it is still cached" "$out" "(cached)"
out=$("$kb" build layout --target testtarget 2>&1)
contains "the one that read it is rebuilt" "$out" "build layout 1"
rm "$repo/recipes/layout.toml"

echo "10. a recipe with no upstream and no script is refused"
cat > "$repo/recipes/layout.toml" <<'R'
name = "layout"
version = "1"
kind = "target"

[build]
system = "make"
R
out=$("$kb" build layout --target testtarget 2>&1 || true)
contains "names the rule" "$out" 'no [source] must use system = "shell"'
rm "$repo/recipes/layout.toml"

echo "11. what a recipe removes must be under \$OUT, and must exist"
write_recipe "$sum" 'remove = ["../escape"]'
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "an escaping path is refused" "$out" "not a plain path relative to \$OUT"
write_recipe "$sum" 'remove = ["usr/bin/never-installed"]'
out=$("$kb" build hello --target testtarget 2>&1 || true)
contains "a stale entry fails the build" "$out" "cannot remove"
write_recipe "$sum" ""

echo
if [ "$fails" -eq 0 ]; then
    echo "engine.sh: all checks passed"
else
    echo "engine.sh: $fails check(s) failed"
    exit 1
fi

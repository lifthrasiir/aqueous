#!/bin/sh
# End-to-end test of merging real modules into the shim with no loader (Windows x86-64):
# records the applications of the given archives (aqr-dll-shim), builds each module they use for
# the no-TLS custom target (NOTLS), merges those libraries into the shim (`aqr-ld merge`), and
# checks that the merged shim decodes every case from its embedded modules with the same bytes as
# running them natively. Pass one or more .aqr archives.
set -eu
cd "$(dirname "$0")/../.."
[ $# -gt 0 ] || { echo "usage: merge-test.sh ARCHIVE.aqr..." >&2; exit 1; }

cargo build --release -p aqr-ld -p aqr-dll-shim
shim=target/release/aqr-dll-shim.exe
ld=target/release/aqr-ld.exe
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

"$shim" record "$dir/cases" "$@"
# the modules the cases use, built for the no-TLS target and digested into blobs for the merge,
# with the shared libraries they link against (the imports between them bound)
mods=$(cat "$dir"/cases/*/info | sed -n 's/^module //p' | sort -u)
NOTLS=1 crates/aqr-dll/build.sh $mods
libs=$(for d in crates/aqr-dll/shared/*/; do echo "aqr_dll_$(basename "$d" | tr - _)"; done)
# shellcheck disable=SC2046
"$ld" blobs --out "$dir/blobs" $(for l in $libs; do echo "target/aqr-dll/$l.dll"; done) \
    $(for m in $mods; do echo "target/aqr-dll/$m.dll"; done)
args=""
for m in $mods; do
    args="$args $m=$dir/blobs/$m.blob"
done
for l in $libs; do
    args="$args lib:$l.dll=$dir/blobs/$l.blob"
done
# shellcheck disable=SC2086
"$ld" merge --stub "$shim" --out "$dir/merged.exe" $args
cp "$dir/merged.exe" "$dir/standalone.exe" # (no aqr-dll/ beside it: only its embedded modules)

bad=0
for case in "$dir"/cases/*/; do
    out=$("$dir/standalone.exe" run "$case" 2>&1) || true
    echo "$out"
    echo "$out" | grep -q ': same$' || bad=$((bad + 1))
done
[ "$bad" = 0 ] || { echo "merge-test: $bad cases not the same" >&2; exit 1; }
echo "merge-test: ok ($(echo "$mods" | wc -w) modules merged, every case decoded with no loader)"

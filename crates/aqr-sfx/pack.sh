#!/bin/sh
# Gathers what `aqr --format sfx` embeds (Windows x86-64) into target/aqr-sfx/, which aqr's build
# script packs into the binary:
#
# - aqr-sfx-shim.exe, the stub (release);
# - aqr-interp.blob, the interpreter blob (aqr-interp built for the no-TLS target, digested by
#   `aqr-ld blob`), for the modules with no native blob;
# - modules/NAME.blob, each module of aqr-modules built as a native library for the no-TLS target
#   (crates/aqr-dll/build.sh with NOTLS=1), and libs/LIB.blob, the shared libraries the modules
#   link against (crates/aqr-dll/layout.py), all digested together by `aqr-ld blobs` (the
#   imports between them bound); and modules.txt: each NAME and the SHA-512/256 of its module
#   when packed (crates/aqr-modules/src/known.rs), so that aqr uses a blob only for the module it
#   was built from;
# - pack.id, a digest of the files.
#
# The modules named are packed (all by default). It needs a nightly toolchain and build-std (as
# NOTLS does). Rerun it after crates/aqr-modules/build.sh, then rebuild aqr (`cargo aqr`).
set -eu
cd "$(dirname "$0")/../.."
case $(uname -s) in
    MINGW*|MSYS*|CYGWIN*) ;;
    *) echo "pack.sh: Windows only (the blobs are PE images)" >&2; exit 1 ;;
esac
out=target/aqr-sfx

cargo build -q --release -p aqr-ld -p aqr-sfx-shim
ld=target/release/aqr-ld.exe
# (all of them, for build.sh to cut the shared libraries to what they use)
NOTLS=1 crates/aqr-dll/build.sh "$@"
[ $# -gt 0 ] || set -- $(cd crates/aqr-modules/src/bin && ls *.rs | sed 's/\.rs$//')
target=crates/aqr-dll/x86_64-aqr-windows-msvc.json
# (linked by aqr-ld as build.sh links, so that the standard library built for the target is not
# built again for a linker of its own)
CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER=$(cygpath -w "$PWD/$ld") \
    cargo +nightly build -q --release -p aqr-interp --target $target -Z json-target-spec -Z build-std

rm -rf "$out"
mkdir -p "$out/modules" "$out/libs"
cp target/release/aqr-sfx-shim.exe "$out/"
"$ld" blob --apply aqr_interp_vtable --info - --out "$out/aqr-interp.blob" \
    target/x86_64-aqr-windows-msvc/release/aqr_interp.dll
libs=$(for d in crates/aqr-dll/shared/*/; do echo "target/aqr-dll/aqr_dll_$(basename "$d" | tr - _).dll"; done)
# shellcheck disable=SC2046,SC2086
"$ld" blobs --out "$out/all" $libs $(for m in "$@"; do echo "target/aqr-dll/$m.dll"; done)
mv "$out"/all/aqr_dll_*.blob "$out/libs/"
mv "$out"/all/*.blob "$out/modules/"
rmdir "$out/all"
for m in "$@"; do
    # (the entry whose block is the binary's: a name of known.rs may have a / for the binary's -)
    hash=$(awk -v b="/$m.block\")," '/sha512_256/ { h = $0; gsub(/.*hex\("|"\).*/, "", h) }
        index($0, b) { print h; exit }' crates/aqr-modules/src/known.rs)
    [ -n "$hash" ] || { echo "pack.sh: $m not in known.rs" >&2; exit 1; }
    echo "$m $hash" >> "$out/modules.txt"
done
ls "$out/libs" > "$out/libs.txt"
# what the pack is, for builds to tell one from another (aqr includes it, so that its build is
# not taken for one of another pack)
(cd "$out" && find . -type f | sort | xargs sha256sum | sha256sum | cut -d' ' -f1) > "$out/pack.id"
echo "$out: $(du -sb "$out" | cut -f1) bytes, $# modules, $(wc -l < "$out/libs.txt") shared libraries"
echo "  modules $(cat "$out"/modules/*.blob | wc -c), shared libraries $(cat "$out"/libs/*.blob | wc -c) bytes"

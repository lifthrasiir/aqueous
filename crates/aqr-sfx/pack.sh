#!/bin/sh
# Gathers what `aqr --format sfx` embeds (Windows x86-64) into target/aqr-sfx/, which aqr's build
# script packs into the binary:
#
# - aqr-sfx-shim.exe, the stub (release);
# - aqr-interp.blob, the interpreter blob (aqr-interp built for the no-TLS target, digested by
#   `aqr-ld blob`), for the modules with no native blob;
# - modules/NAME.blob, each module of aqr-modules built as a native library for the no-TLS target
#   (crates/aqr-dll/build.sh with NOTLS=1) and digested, and modules.txt: each NAME and the
#   SHA-512/256 of its module when packed (crates/aqr-modules/src/known.rs), so that aqr uses a
#   blob only for the module it was built from;
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
[ $# -gt 0 ] || set -- $(cd crates/aqr-modules/src/bin && ls *.rs | sed 's/\.rs$//')

cargo build -q --release -p aqr-ld -p aqr-sfx-shim
ld=target/release/aqr-ld.exe
target=crates/aqr-dll/x86_64-aqr-windows-msvc.json
cargo +nightly build -q --release -p aqr-interp --target $target -Z json-target-spec -Z build-std
NOTLS=1 crates/aqr-dll/build.sh "$@"

rm -rf "$out"
mkdir -p "$out/modules"
cp target/release/aqr-sfx-shim.exe "$out/"
"$ld" blob --apply aqr_interp_vtable --info - --out "$out/aqr-interp.blob" \
    target/x86_64-aqr-windows-msvc/release/aqr_interp.dll
for m in "$@"; do
    "$ld" blob --out "$out/modules/$m.blob" "target/aqr-dll/$m.dll"
    # (the entry whose block is the binary's: a name of known.rs may have a / for the binary's -)
    hash=$(awk -v b="/$m.block\")," '/sha512_256/ { h = $0; gsub(/.*hex\("|"\).*/, "", h) }
        index($0, b) { print h; exit }' crates/aqr-modules/src/known.rs)
    [ -n "$hash" ] || { echo "pack.sh: $m not in known.rs" >&2; exit 1; }
    echo "$m $hash" >> "$out/modules.txt"
done
# what the pack is, for builds to tell one from another (aqr includes it, so that its build is
# not taken for one of another pack)
(cd "$out" && find . -type f | sort | xargs sha256sum | sha256sum | cut -d' ' -f1) > "$out/pack.id"
echo "$out: $(du -sb "$out" | cut -f1) bytes, $# modules"

#!/bin/sh
# Builds the modules named (binaries of crates/aqr-modules, `lz4-recons`; all of them by
# default) as native libraries into target/aqr-dll/, one after another: each a build of this
# crate with AQR_DLL_MODULE set, the crates under it compiled once. PROFILE (release by default)
# is the profile built with.
#
# With NOTLS=1 (Windows), the libraries are built for the custom target
# x86_64-aqr-windows-msvc.json, which has native thread-locals off so the standard library's
# thread-locals use TlsAlloc rather than a TLS directory (one per image, not relocatable). Such a
# library is mergeable into a stub by `aqr-ld merge` (its code uses no TLS directory, only
# imports). It needs a nightly toolchain and build-std, so it is slower.
set -eu
cd "$(dirname "$0")/../.."
profile=${PROFILE:-release}
case $(uname -s) in
    MINGW*|MSYS*|CYGWIN*) lib=aqr_dll.dll ext=dll ;;
    Darwin) lib=libaqr_dll.dylib ext=dylib ;;
    *) lib=libaqr_dll.so ext=so ;;
esac
if [ "${NOTLS:-}" = 1 ]; then
    target=crates/aqr-dll/x86_64-aqr-windows-msvc.json
    pre="+nightly"
    post="--target $target -Z json-target-spec -Z build-std"
    built=target/x86_64-aqr-windows-msvc/$profile
else
    pre=
    post=
    built=target/$profile
fi
[ $# -gt 0 ] || set -- $(cd crates/aqr-modules/src/bin && ls *.rs | sed 's/\.rs$//')
mkdir -p target/aqr-dll
for m in "$@"; do
    [ -e "crates/aqr-modules/src/bin/$m.rs" ] || { echo "build.sh: no module $m" >&2; exit 1; }
    AQR_DLL_MODULE=$m cargo $pre build -q --profile "$profile" -p aqr-dll $post
    cp "$built/$lib" "target/aqr-dll/$m.$ext"
    echo "target/aqr-dll/$m.$ext: $(wc -c < "target/aqr-dll/$m.$ext") bytes"
done

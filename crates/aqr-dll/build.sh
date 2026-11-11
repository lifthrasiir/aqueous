#!/bin/sh
# Builds the modules named (binaries of crates/aqr-modules, `lz4-recons`; all of them by
# default) as native libraries into target/aqr-dll/, one after another: each a build of this
# crate with AQR_DLL_MODULE set, the crates under it compiled once. PROFILE (release by default)
# is the profile built with.
set -eu
cd "$(dirname "$0")/../.."
profile=${PROFILE:-release}
case $(uname -s) in
    MINGW*|MSYS*|CYGWIN*) lib=aqr_dll.dll ext=dll ;;
    Darwin) lib=libaqr_dll.dylib ext=dylib ;;
    *) lib=libaqr_dll.so ext=so ;;
esac
[ $# -gt 0 ] || set -- $(cd crates/aqr-modules/src/bin && ls *.rs | sed 's/\.rs$//')
mkdir -p target/aqr-dll
for m in "$@"; do
    [ -e "crates/aqr-modules/src/bin/$m.rs" ] || { echo "build.sh: no module $m" >&2; exit 1; }
    AQR_DLL_MODULE=$m cargo build -q --profile "$profile" -p aqr-dll
    cp "target/$profile/$lib" "target/aqr-dll/$m.$ext"
    echo "target/aqr-dll/$m.$ext: $(wc -c < "target/aqr-dll/$m.$ext") bytes"
done

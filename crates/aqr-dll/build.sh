#!/bin/sh
# Builds the modules named (binaries of crates/aqr-modules, `lz4-recons`; all of them by
# default) as native libraries into target/aqr-dll/, one after another: each a build of this
# crate with AQR_DLL_MODULE set, the crates under it compiled once. PROFILE (release by default)
# is the profile built with.
#
# The crates that several modules use are in shared libraries (layout.py, run first: the dylibs
# of shared/, the runtime aqr-dll-rt among them), built with the first module and copied into
# target/aqr-dll/ beside the modules (`aqr_dll_NAME.dll`); each module's library links
# against them.
#
# With NOTLS=1 (Windows), the libraries are built for the custom target
# x86_64-aqr-windows-msvc.json, which has native thread-locals off so the standard library's
# thread-locals use TlsAlloc rather than a TLS directory (one per image, not relocatable). Such a
# library is mergeable into a stub by `aqr-ld merge` (its code uses no TLS directory, only
# imports). It needs the nightly toolchain of nightly-toolchain (AQR_NIGHTLY to use another) and
# build-std, so it is slower. aqr-ld links them
# (src/msvc.rs): building every module, the shared libraries are linked again exporting only
# what the others import from them and what rustc has every link reference (`aqr-ld exports`,
# target/aqr-dll/exports/ and needed/), the rest of their
# code left out, until that no longer changes (each one less imports from the others).
set -eu
cd "$(dirname "$0")/../.."
profile=${PROFILE:-release}
case $(uname -s) in
    MINGW*|MSYS*|CYGWIN*) lib=aqr_dll.dll ext=dll pre_lib= ;;
    Darwin) lib=libaqr_dll.dylib ext=dylib pre_lib=lib ;;
    *) lib=libaqr_dll.so ext=so pre_lib=lib ;;
esac
all=
[ $# -gt 0 ] || { all=1; set -- $(cd crates/aqr-modules/src/bin && ls *.rs | sed 's/\.rs$//'); }
exports=target/aqr-dll/exports
needed=target/aqr-dll/needed
if [ "${NOTLS:-}" = 1 ]; then
    target=crates/aqr-dll/x86_64-aqr-windows-msvc.json
    # (the nightly of nightly-toolchain, or AQR_NIGHTLY's)
    pre="+${AQR_NIGHTLY:-$(cat nightly-toolchain)}"
    post="--target $target -Z json-target-spec -Z build-std"
    built=target/x86_64-aqr-windows-msvc/$profile
    cargo build -q --release -p aqr-ld
    CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER=$(cygpath -w "$PWD/target/release/aqr-ld.exe")
    AQR_LD_EXPORTS=$(cygpath -w "$PWD/$exports")
    AQR_LD_NEEDED=$(cygpath -w "$PWD/$needed")
    export CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER AQR_LD_EXPORTS AQR_LD_NEEDED
else
    pre=
    post=
    built=target/$profile
fi
# (PYTHON, else the venv's Python, else one on PATH, as in CI: layout.py needs nothing else)
python=${PYTHON:-.venv/Scripts/python.exe}
[ -n "${PYTHON:-}" ] || [ -x "$python" ] || python=.venv/bin/python
[ -n "${PYTHON:-}" ] || [ -x "$python" ] || python=$(command -v python3 || command -v python) ||
    { echo "build.sh: no Python (set PYTHON)" >&2; exit 1; }
"$python" -I crates/aqr-dll/layout.py > /dev/null
mkdir -p target/aqr-dll
# (the shared libraries linked whole first: what the modules import from them is what is found)
rm -rf "$exports" "$needed"
touch crates/aqr-dll/shared/*/src/lib.rs
module() {
    AQR_DLL_MODULE=$1 cargo $pre build -q --profile "$profile" -p aqr-dll $post
    cp "$built/$lib" "target/aqr-dll/$1.$ext"
}
libs() {
    for d in crates/aqr-dll/shared/*/; do
        echo "target/aqr-dll/aqr_dll_$(basename "$d" | tr - _).$ext"
    done
}
copy_libs() {
    for l in $(libs); do
        cp "$built/${pre_lib}$(basename "$l")" target/aqr-dll/
    done
}
for m in "$@"; do
    [ -e "crates/aqr-modules/src/bin/$m.rs" ] || { echo "build.sh: no module $m" >&2; exit 1; }
    module "$m"
    echo "target/aqr-dll/$m.$ext: $(wc -c < "target/aqr-dll/$m.$ext") bytes"
done
copy_libs
whole=$(cat $(libs) | wc -c)
if [ "${NOTLS:-}" = 1 ] && [ -n "$all" ]; then
    # (the modules imported from as they are; only the shared libraries linked again, by the
    # first module's build)
    mods=$(for m in "$@"; do echo "target/aqr-dll/$m.dll"; done)
    for round in 1 2 3 4 5 6; do
        # shellcheck disable=SC2046,SC2086
        target/release/aqr-ld.exe exports --out "$exports.new" --needed "$needed" $(libs) $mods
        if [ -d "$exports" ] && diff -rq "$exports" "$exports.new" > /dev/null; then
            rm -rf "$exports.new"
            break
        fi
        rm -rf "$exports"
        mv "$exports.new" "$exports"
        touch crates/aqr-dll/shared/*/src/lib.rs
        module "$1"
        copy_libs
        echo "build.sh: round $round, the shared libraries $(cat $(libs) | wc -c) bytes"
    done
fi
echo "target/aqr-dll: $(ls crates/aqr-dll/shared | wc -l) shared libraries, $(cat $(libs) | wc -c) bytes (whole $whole)"

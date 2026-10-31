#!/bin/sh
# Builds Aqueous.app (aqr-mac and aqr, the dist profile) and Aqueous.zip in target/dist:
#
#   crates/aqr-mac/bundle.sh [--no-build] [--universal]
#
# --no-build takes the binaries of target/dist as they are. --universal builds for x86_64 as
# well and joins the two (lipo), about twice the size. The bundle is signed ad hoc (codesign
# -s -): Gatekeeper still asks of it downloaded, as it is not notarized.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
here=$root/crates/aqr-mac
build=1 universal=0
for a in "$@"; do
    case $a in
    --no-build) build=0 ;;
    --universal) universal=1 ;;
    *) echo "usage: $0 [--no-build] [--universal]" >&2; exit 2 ;;
    esac
done
cd "$root"
dist=$root/target/dist
if [ "$universal" = 1 ]; then
    targets="aarch64-apple-darwin x86_64-apple-darwin"
    if [ "$build" = 1 ]; then
        for t in $targets; do cargo build --profile dist --target "$t" -p aqr -p aqr-mac; done
    fi
    mkdir -p "$dist/universal"
    for b in aqr aqr-mac; do
        lipo -create -output "$dist/universal/$b" "$root/target/aarch64-apple-darwin/dist/$b" \
            "$root/target/x86_64-apple-darwin/dist/$b"
    done
    bin=$dist/universal
else
    [ "$build" = 1 ] && cargo build --profile dist -p aqr -p aqr-mac
    bin=$dist
fi
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/Cargo.toml" | head -n 1)
app=$dist/Aqueous.app
rm -rf "${app:?}"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "$bin/aqr-mac" "$bin/aqr" "$app/Contents/MacOS/"
cp "$root/droplet.icns" "$app/Contents/Resources/"
codesign --force --sign - "$app/Contents/MacOS/aqr"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
rm -f "$dist/Aqueous.zip"
# (ditto, as Finder's Compress: the bundle as one folder, its attributes kept)
ditto -c -k --keepParent "$app" "$dist/Aqueous.zip"
ls -l "$app/Contents/MacOS" "$dist/Aqueous.zip"

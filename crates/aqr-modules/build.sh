#!/bin/sh
# Builds the modules (src/bin) through aqr-ld into wasm/, and src/known.rs from them, with the
# versions of aqr-decompress's modules, as well as the spec's table of the registry's initial
# contents (the crate of a module's document from the first line of its source). Nothing else
# is rebuilt from them: the bytes kept are the modules (a toolchain giving other bytes makes
# other modules, of other hashes).
set -eu
cd "$(dirname "$0")/../.."
cargo build --release -p aqr-ld
# (paths in the modules, of panics, made the same anywhere, and without the workspace's
# directories; the crates' paths still reach the symbols, so moving a crate can reorder functions)
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=crates/= --remap-path-prefix=codecs/=" \
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER="$PWD/target/release/aqr-ld" \
    cargo build --profile release-wasm --target wasm32-unknown-unknown -p aqr-modules -F wasm --bins
built=target/wasm32-unknown-unknown/release-wasm
modules=$(cargo run -q --release -p aqr-decompress --example aqr-decompress-modules)
known=crates/aqr-modules/src/known.rs
spec=spec/index.bs
rows="$known.rows"
: > "$rows"
{
    echo "// Made by build.sh."
    echo
    echo "use crate::{Known, hex};"
    echo
    echo "pub const KNOWN: &[Known] = &["
    for src in crates/aqr-modules/src/bin/*.rs; do
        bin=$(basename "$src" .rs)
        line=$(echo "$modules" | awk -v b="$bin" '$3 == b')
        [ -n "$line" ] || { echo "build.sh: $bin is no module of aqr-decompress" >&2; exit 1; }
        set -- $line
        install -m 644 "$built/$bin.wasm" crates/aqr-modules/wasm/
        sum=$(shasum -a 512256 "crates/aqr-modules/wasm/$bin.wasm" | cut -c1-64)
        echo "    Known {"
        echo "        name: \"$1\","
        echo "        version: $2,"
        echo "        sha512_256: hex(\"$sum\"),"
        echo "        block: include_bytes!(\"../wasm/$bin.block\"),"
        echo "    },"
        crate=$(head -1 "$src" | sed -n 's/^\/\/!.*(\(aqr-[a-z0-9-]*\)).*/\1/p')
        [ -n "$crate" ] || { echo "build.sh: no crate in the first line of $src" >&2; exit 1; }
        {
            echo "    <tr>"
            echo "      <td><code>$sum</code>"
            echo "      <td>$1@$2"
            echo "      <td><a href=\"https://github.com/lifthrasiir/aqueous/blob/main/codecs/$crate/FORMAT.md\">codecs/$crate/FORMAT.md</a>"
        } >> "$rows"
    done
    echo "];"
} > "$known.new"
# the data of their module blocks, compressed once
cargo run -q --release -p aqr-decompress --example aqr-decompress-modules -- crates/aqr-modules/wasm/*.wasm
mv "$known.new" "$known"
# (the rows of the first table after the registry's heading replaced)
awk -v rows="$rows" '
    /^<h3 id=modules-registry>/ { sect = 1 }
    sect == 1 && /^  <tbody>$/ { print; while ((getline line < rows) > 0) print line; sect = 2; skip = 1; next }
    skip && /^  <\/tbody>$/ { skip = 0 }
    !skip { print }
    END { if (sect != 2) { print "build.sh: no table of the modules registry in the spec" > "/dev/stderr"; exit 1 } }
' "$spec" > "$spec.new"
mv "$spec.new" "$spec"
rm "$rows"
# (made before the crates compiled above, which it would not rebuild by its time)
touch "$known"
# (the files of modules no longer built; the others were overwritten, as known.rs had them)
for f in crates/aqr-modules/wasm/*.wasm crates/aqr-modules/wasm/*.block; do
    [ -e "crates/aqr-modules/src/bin/$(basename "${f%.*}").rs" ] || rm -f "$f"
done
ls -l crates/aqr-modules/wasm

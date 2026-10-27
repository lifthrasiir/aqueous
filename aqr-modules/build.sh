#!/bin/sh
# Builds the modules (src/bin) through aqr-ld into wasm/, and src/known.rs from them, with the
# versions of aqr-decompress's modules. Nothing else is rebuilt from them: the bytes kept are
# the modules (a toolchain giving other bytes makes other modules, of other hashes).
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p aqr-ld
# (paths in the modules, of panics, made the same anywhere)
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo" \
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER="$PWD/target/release/aqr-ld" \
    cargo build --profile release-wasm --target wasm32-unknown-unknown -p aqr-modules -F wasm --bins
built=target/wasm32-unknown-unknown/release-wasm
modules=$(cargo run -q --release -p aqr-decompress --example aqr-decompress-modules)
rm -f aqr-modules/wasm/*.wasm
known=aqr-modules/src/known.rs
{
    echo "// Made by build.sh."
    echo
    echo "use crate::{Known, hex};"
    echo
    echo "pub const KNOWN: &[Known] = &["
    for src in aqr-modules/src/bin/*.rs; do
        bin=$(basename "$src" .rs)
        line=$(echo "$modules" | awk -v b="$bin" '$3 == b')
        [ -n "$line" ] || { echo "build.sh: $bin is no module of aqr-decompress" >&2; exit 1; }
        set -- $line
        install -m 644 "$built/$bin.wasm" aqr-modules/wasm/
        sum=$(shasum -a 512256 "aqr-modules/wasm/$bin.wasm" | cut -c1-64)
        echo "    Known {"
        echo "        name: \"$1\","
        echo "        version: $2,"
        echo "        sha512_256: hex(\"$sum\"),"
        echo "        wasm: include_bytes!(\"../wasm/$bin.wasm\"),"
        echo "    },"
    done
    echo "];"
} > "$known.new"
mv "$known.new" "$known"
ls -l aqr-modules/wasm

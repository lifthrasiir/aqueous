#!/bin/sh
# Builds aqr-ld and the demos through it, then runs them on Node (host.mjs).
set -e
cd "$(dirname "$0")/../.."
cargo build --release -p aqr-ld
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER="$PWD/target/release/aqr-ld" \
    cargo build --profile release-wasm --target wasm32-unknown-unknown -p aqr-demo-concat -p aqr-demo-xor -p aqr-demo-split
node --experimental-wasm-type-reflection aqr-api/demo/e2e.mjs target/wasm32-unknown-unknown/release-wasm

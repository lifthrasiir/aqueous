@cargo build --profile release-wasm --no-default-features -F decode --target wasm32-unknown-unknown
@zstd -19 --no-check -f target\wasm32-unknown-unknown\release-wasm\exe_recons.wasm
@dir target\wasm32-unknown-unknown\release-wasm\exe_recons.wasm.zst

@rem (the cdylib here alone: as a crate type of the package, every native build made a DLL too)
@cargo rustc --lib --profile release-wasm --no-default-features -F decode --target wasm32-unknown-unknown --crate-type cdylib
@zstd -19 --no-check -f ..\..\target\wasm32-unknown-unknown\release-wasm\aqr_exe.wasm
@dir ..\..\target\wasm32-unknown-unknown\release-wasm\aqr_exe.wasm.zst

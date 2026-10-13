#!/bin/sh
# Retrains the built-in tables on FMA dirs 000-019 and reports the ratio on 100-104.
# The trained model replaces model/builtin.bin (restore it with git if unwanted).
set -e
cd "$(dirname "$0")/.."
F=corpus/fma/fma_small
cargo build --release -q
./target/release/train -a model/builtin.bin $(printf "$F/%03d " $(seq 0 19)) 2>&1
cargo build --release -q
./target/release/mp3r bench $F/100 $F/101 $F/102 $F/103 $F/104 | head -1

#!/bin/sh
# Runs fuzz target $1 (decode, salvage, envelope, roundtrip, unpack,
# modules, pack, sfx, natives, wasm, compress) with its seeds (and the dictionary of
# archives), further arguments to libFuzzer:
# ./run.sh decode -max_total_time=600 -fork=8 -ignore_crashes=1
set -eu
cd "$(dirname "$0")"
t=${1:?target}
shift
[ -d corpus/"$t" ] || cargo +nightly run -q --release --features tools --bin seeds -- "$t" corpus/"$t"
case $t in
    compress) opts="-max_len=262144 -timeout=120" ;;
    sfx) opts="-max_len=131072 -timeout=30" ;;
    pack) opts="-max_len=65536 -timeout=30" ;;
    modules) opts="-max_len=196608 -timeout=60" ;;
    unpack) opts="-max_len=262145 -timeout=60" ;;
    roundtrip) opts="-max_len=16384 -timeout=10" ;;
    envelope) opts="-dict=aqr.dict -max_len=327680 -timeout=10" ;;
    wasm) opts="-dict=aqr.dict -max_len=524288 -timeout=60" ;;
    natives) opts="-dict=aqr.dict -max_len=524288 -timeout=30" ;;
    *) opts="-dict=aqr.dict -max_len=65536 -timeout=10" ;;
esac
# (ASan's redzones around globals would pad inventory's constructors in __mod_init_func, which
# ld then takes for initializers of no target)
export RUSTFLAGS="${RUSTFLAGS:-} -Cllvm-args=-asan-globals=0"
export ASAN_OPTIONS=${ASAN_OPTIONS:-allocator_may_return_null=1}
# shellcheck disable=SC2086
exec cargo +nightly fuzz run -O "$t" corpus/"$t" -- $opts -use_value_profile=1 \
    -rss_limit_mb=4096 -malloc_limit_mb=4096 "$@"

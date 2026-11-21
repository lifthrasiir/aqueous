#!/bin/sh
# Runs fuzz target $1 (decode, salvage) with the seeds and the dictionary, further arguments to
# libFuzzer: ./run.sh decode -max_total_time=600 -fork=8 -ignore_crashes=1
set -eu
cd "$(dirname "$0")"
t=${1:?target}
shift
[ -d corpus/"$t" ] || cargo +nightly run -q --release --bin seeds -- corpus/"$t"
export ASAN_OPTIONS=${ASAN_OPTIONS:-allocator_may_return_null=1}
exec cargo +nightly fuzz run -O "$t" corpus/"$t" -- -dict=aqr.dict -use_value_profile=1 \
    -max_len=65536 -rss_limit_mb=4096 -malloc_limit_mb=4096 -timeout=10 "$@"

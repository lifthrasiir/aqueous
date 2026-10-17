#!/bin/bash
# Runs zstd_dict_bench.py over several corpora of many small files; writes zstd-dict-<name>.{md,json}.
# Only github_users is fully pinned; the others are local installs whose fingerprint
# (content hash) is printed to zstd-dict-<name>.log and recorded in the JSON.
#
# Usage: ./zstd-dict-run-all.sh [name...]   (default: all)
set -euo pipefail
cd "$(dirname "$0")"
PY=.venv/bin/python

GH_URL=https://github.com/facebook/zstd/releases/download/v1.1.3/github_users_sample_set.tar.zst
GH_SHA=e35074abe4db6a888f19bcc12ad56c760bec2ca86bb7dff18bb4967789fc0de1
fetch_github() {
    [ -d data/github_users/github ] && return
    mkdir -p data
    [ -f data/github_users_sample_set.tar.zst ] || curl -sSL -o data/github_users_sample_set.tar.zst "$GH_URL"
    echo "$GH_SHA  data/github_users_sample_set.tar.zst" | shasum -a 256 -c -
    rm -rf data/github_users.tmp && mkdir data/github_users.tmp
    zstd -dc data/github_users_sample_set.tar.zst | tar xf - -C data/github_users.tmp
    mv data/github_users.tmp data/github_users
}

PYLIB=$(ls -d /opt/homebrew/Cellar/python@3.14/*/Frameworks/Python.framework/Versions/3.14/lib/python3.14 | sort -V | tail -1)
BIG=''  # zstd_dict_bench.py defaults
SMALL='--dict-sizes 4K,16K,64K --packs 0,4K,16K,64K,256K,1M,all --time-frac 1'

run() {
    local name=$1; shift
    echo "== $name" >&2
    $PY zstd_dict_bench.py "$@" --out "zstd-dict-$name.json" > "zstd-dict-$name.md" 2> "zstd-dict-$name.log"
    cat "zstd-dict-$name.md"
}

all=(rustdoc cargo pystdlib sdk-headers github-users)
for name in "${@:-${all[@]}}"; do
    case $name in
    rustdoc)      run $name $BIG ;;
    cargo)        run $name $BIG --corpus ~/.cargo/registry/src --glob '*.rs' ;;
    pystdlib)     run $name $BIG --corpus "$PYLIB" --glob '*.py' --time-frac 0.3 ;;
    sdk-headers)  run $name $BIG --corpus /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk --glob '*.h' ;;
    github-users) fetch_github; run $name $SMALL --tune-size 16K --train-frac 0.3 --corpus data/github_users --glob '*.json' ;;
    *) echo "unknown dataset $name" >&2; exit 1 ;;
    esac
done

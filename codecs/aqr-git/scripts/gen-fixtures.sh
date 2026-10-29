#!/bin/sh
# Makes tests/data/small.{pack,idx,rev}: the one pack of a history of ten commits of text
# that changes a little each time, with its index and reverse index, as the git CLI writes
# them (other versions of git or zlib may write other bytes).
set -eu
data="$(cd "$(dirname "$0")/.." && pwd)/tests/data"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cd "$dir"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_DATE=2020-01-01T00:00:00Z GIT_COMMITTER_DATE=2020-01-01T00:00:00Z
g() { git -c user.name=T -c user.email=t@example.com -c commit.gpgsign=false -c init.defaultBranch=main "$@"; }
g init -q
for c in 0 1 2 3 4 5 6 7 8 9; do
    awk -v c=$c 'BEGIN { for (i = 0; i < 300; i++) {
        if (i % 7 == c) print "changed in " c; else { x = ""; for (k = 0; k < i % 40; k++) x = x "x"; print "line " i " " x } } }' > a.txt
    awk -v c=$c 'BEGIN { for (i = 0; i < 50 + c; i++) print "entry " i * c " of " c }' > b$((c % 3)).txt
    g add -A
    g commit -q -m "commit $c"
done
g -c pack.writeReverseIndex=true repack -q -a -d
mkdir -p "$data"
for e in pack idx rev; do
    cp .git/objects/pack/pack-*.$e "$data/small.$e"
    chmod 644 "$data/small.$e"
done

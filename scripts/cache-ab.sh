#!/bin/zsh
# A/B of the decoders tuned to the caches, before and after their tuning was made to hold on
# cores other than Apple's (lines of 64 bytes, allocators not aligning large blocks): aqr-fasta's
# prefetch of a Line of contexts, aqr-gif's buckets aligned.
#
# Usage: scripts/cache-ab.sh [-r ROUNDS] [-j JOBS] FASTA_FILE... -- GIF_FILE_OR_DIR...
#   (either side may be empty). BEFORE and AFTER (revisions) default to 4fecd60 and HEAD.
#
# Each revision is taken by `git archive` (no worktree) into target/cache-ab/src-<rev>, and its
# aqr-fasta-bench and aqr-gif-check built there once, kept as target/cache-ab/bin-<rev>/ (a
# target directory each: one shared took the second tree's crates for built, as cargo hashes
# them by their path in the workspace and git archive dates the files by the commit). The runs alternate: before, after, before, … ROUNDS times (3), each
# alone (with /usr/bin/time -l's instructions and cycles where it tells them) and JOBS at once
# (all cores by default: the speed of files decoded together, which the model sizes were picked
# for). The faster figure is what counts; all are kept in target/cache-ab/results-<time>.txt.
# Inputs of a few minutes a run at most: chr1 of hg19 rather than all of it.
set -eu
cd "${0:A:h}/.."
root=$PWD
before=${BEFORE:-4fecd60}
after=${AFTER:-HEAD}
rounds=3
jobs=$( (sysctl -n hw.ncpu || nproc) 2>/dev/null )
while getopts r:j: o; do
    case $o in
        r) rounds=$OPTARG ;;
        j) jobs=$OPTARG ;;
        *) echo "usage: $0 [-r ROUNDS] [-j JOBS] FASTA... -- GIF..." >&2; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
fasta=() gif=() side=fasta
for a in "$@"; do
    if [[ $a == -- ]]; then side=gif; continue; fi
    if [[ $side == fasta ]]; then fasta+=("${a:A}"); else gif+=("${a:A}"); fi
done
(( ${#fasta} + ${#gif} )) || { echo "$0: no inputs" >&2; exit 2; }

dir=$root/target/cache-ab
mkdir -p "$dir"
out=$dir/results-$(date +%Y%m%d-%H%M%S).txt

# bin-<rev>/{aqr-fasta-bench,aqr-gif-check}, built once
build() {
    local rev=$(git rev-parse --short "$1")
    local bin=$dir/bin-$rev
    if [[ ! -x $bin/aqr-gif-check ]]; then
        local src=$dir/src-$rev
        rm -rf "${src:?}"
        mkdir -p "$src"
        git archive "$rev" | tar -x -C "$src"
        (cd "$src" && CARGO_TARGET_DIR=$src/target cargo build --release -p aqr-fasta -p aqr-gif \
            --example aqr-fasta-bench --example aqr-gif-check >&2)
        mkdir -p "$bin"
        cp "$src/target/release/examples/aqr-fasta-bench" "$src/target/release/examples/aqr-gif-check" "$bin/"
        rm -rf "${src:?}"
    fi
    echo "$bin"
}
b0=$(build "$before")
b1=$(build "$after")

say() { print -r -- "$*" | tee -a "$out"; }
say "cache-ab: before $before ($b0:t), after $after ($b1:t), $rounds rounds, $jobs at once; $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)"

# one run alone: the tool's own line, and the instructions and cycles of /usr/bin/time -l
alone() {
    local t=$dir/time.$$
    local line
    if [[ $(uname) == Darwin ]]; then
        line=$(/usr/bin/time -l "$@" 2>"$t" | tail -1)
    else
        line=$("$@" 2>"$t" | tail -1)
    fi
    local ic=$(awk '/instructions retired/ { i = $1 } /cycles elapsed/ { c = $1 } END { if (i) printf "%.1f G instr, %.1f G cycles", i / 1e9, c / 1e9 }' "$t")
    rm -f "$t"
    print -r -- "$line${ic:+ ($ic)}"
}

# `jobs` runs at once: the tools' last lines
together() {
    local i t=$dir/together.$$
    for i in {1..$jobs}; do "$@" | tail -1 > "$t.$i" & done
    wait
    for i in {1..$jobs}; do cat "$t.$i"; rm -f "$t.$i"; done
}

for r in {1..$rounds}; do
    for which in before after; do
        [[ $which == before ]] && b=$b0 || b=$b1
        if (( ${#fasta} )); then
            say "$r $which fasta alone: $(alone "$b/aqr-fasta-bench" "${fasta[@]}" | sed 's/.*encode/encode/')"
            say "$r $which fasta x$jobs, decode MB/s: $(together "$b/aqr-fasta-bench" "${fasta[@]}" | sed 's/.*decode \([0-9]*\).*/\1/' | tr '\n' ' ')"
        fi
        if (( ${#gif} )); then
            say "$r $which gif alone: $(alone "$b/aqr-gif-check" "${gif[@]}" | sed 's/.*; split/split/')"
            say "$r $which gif x$jobs: $(together "$b/aqr-gif-check" "${gif[@]}" | sed 's/.*; split/split/' | tr '\n' ' ')"
        fi
    done
done
say "results in $out"

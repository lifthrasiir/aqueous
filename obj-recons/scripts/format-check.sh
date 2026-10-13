#!/bin/sh
# Checks that compressed outputs are byte-identical to the frozen reference
# (scripts/format-ref.tsv: path, sha256 prefix). usage: scripts/format-check.sh [OBJR]
# scripts/format-check.sh --update OBJR rewrites the reference from the current corpus.
cd "$(dirname "$0")/.."
if [ "${1:-}" = --update ]; then
    objr=${2:-target/release/objr}
    find corpus -name '*.obj' | sort | while read -r f; do
        printf '%s\t%s\n' "$f" "$("$objr" c "$f" /dev/stdout | shasum -a 256 | cut -c1-16)"
    done > scripts/format-ref.tsv
    exit
fi
objr=${1:-target/release/objr}
bad=0
while IFS="	" read -r f h; do
    g=$("$objr" c "$f" /dev/stdout | shasum -a 256 | cut -c1-16)
    [ "$g" = "$h" ] || { echo "CHANGED: $f"; bad=1; }
done < scripts/format-ref.tsv
[ $bad = 0 ] && echo "format unchanged"
exit $bad

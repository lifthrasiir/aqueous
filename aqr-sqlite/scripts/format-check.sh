#!/bin/sh
# Checks that compressed outputs are byte-identical to the frozen reference
# (scripts/format-ref.tsv: path, sha256 prefix). usage: scripts/format-check.sh [SQLR]
sqlr=${1:-../target/release/sqlr}
bad=0
while IFS="	" read -r f h; do
    g=$("$sqlr" c "$f" /dev/stdout | shasum -a 256 | cut -c1-16)
    [ "$g" = "$h" ] || { echo "CHANGED: $f"; bad=1; }
done < scripts/format-ref.tsv
[ $bad = 0 ] && echo "format unchanged"
exit $bad

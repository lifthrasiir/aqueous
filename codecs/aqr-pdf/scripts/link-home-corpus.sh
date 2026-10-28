#!/bin/sh
# Symlinks the PDF files found under $HOME into corpus/home/ (deduplicated by content),
# named by a short hash prefix and the original base name.
set -eu
OUT=${1:-corpus/home}
mkdir -p "$OUT"
cd "$HOME"
perl -e 'alarm 900; exec @ARGV' find . -type f -iname '*.pdf' -size +1k \
    -not -path './Library/Caches/*' -not -path './.Trash/*' -not -path '*/node_modules/*' \
    -not -path './Works/git/aqueous/codecs/aqr-pdf/*' 2>/dev/null |
while IFS= read -r f; do
    h=$(shasum -a 256 "$f" | cut -c1-8)
    ls "$OLDPWD/$OUT"/"$h"-* >/dev/null 2>&1 && continue
    ln -s "$HOME/${f#./}" "$OLDPWD/$OUT/$h-$(basename "$f")"
done
ls -l "$OLDPWD/$OUT" | wc -l

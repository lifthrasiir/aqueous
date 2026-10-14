#!/bin/bash
# Snapshot the repositories under ~/Works/git (or $1) into corpus/local/NAME: the whole .git
# plus the checked-out files git would see (tracked, or untracked and not ignored), so that
# build outputs stay out. APFS clones (cp -c) make it cheap. Idempotent: existing snapshots
# are kept (delete one to refresh it).
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:-$HOME/Works/git}"
OUT="$ROOT/corpus/local"
mkdir -p "$OUT"
for d in "$SRC"/*/; do
  d=${d%/}; name=$(basename "$d")
  [ -d "$d/.git" ] || continue
  dest="$OUT/$name"
  [ -e "$dest" ] && { echo "skip  $name"; continue; }
  tmp="$OUT/.tmp-$name"; rm -rf "$tmp"; mkdir -p "$tmp"
  cp -cRp "$d/.git" "$tmp/.git"
  (cd "$d" && git ls-files -z -co --exclude-standard) | while IFS= read -r -d '' f; do
    [ -e "$d/$f" ] || [ -L "$d/$f" ] || continue
    [ -d "$d/$f" ] && [ ! -L "$d/$f" ] && continue
    mkdir -p "$tmp/$(dirname "$f")"
    cp -cp -P "$d/$f" "$tmp/$f"
  done
  mv "$tmp" "$dest"
  echo "ok    $name ($(du -sk "$dest" | cut -f1) KB)"
done

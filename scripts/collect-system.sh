#!/bin/bash
# Copy SQLite files (by magic header) from non-personal system locations into corpus/system/.
# Idempotent: existing destination files are skipped.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/corpus/system"
mkdir -p "$OUT"
MAXFILE=$((50*1024*1024))
CAP=$((150*1024*1024))
total=$(find "$OUT" -type f -exec stat -f %z {} + 2>/dev/null | awk '{s+=$1} END{print s+0}')
n=0

roots=(/System/Library /Library /usr/share /opt/homebrew)
for app in /Applications/*.app; do [ -d "$app" ] && roots+=("$app"); done

# Candidate files: regular, 512 B..50 MB, not wal/shm/journal, not user-specific paths.
for r in "${roots[@]}"; do
  [ -d "$r" ] || continue
  find "$r" -xdev -type f -size +512c -size -$((MAXFILE/1024+1))k \
       ! -name '*-wal' ! -name '*-shm' ! -name '*-journal' \
       ! -path '*/Users/*' ! -path '/Library/Application Support/*/Users/*' \
       ! -path '/Library/Mail/*' ! -path '/Library/Keychains/*' ! -path '/Library/Preferences/*' \
       ! -path '/Library/Caches/*' ! -path '/Library/Logs/*' ! -path '/Library/Messages/*' \
       ! -path '/Library/Application Support/com.apple.TCC/*' ! -path '*/Keychains/*' \
       -print0 2>/dev/null
done | while IFS= read -r -d '' f; do
  [ -r "$f" ] || continue
  [ "$(head -c 16 "$f" 2>/dev/null | od -An -c | tr -d ' \n')" = 'SQLiteformat3\0' ] || continue
  size=$(stat -f %z "$f") || continue
  [ "$size" -le "$MAXFILE" ] || continue
  # flattened unique name: path with / -> __, plus short hash of the full path
  flat=$(printf '%s' "${f#/}" | tr '/ ' '__' | sed 's/_\{3,\}/__/g')
  h=$(printf '%s' "$f" | shasum | cut -c1-6)
  base=$(basename "$f"); dest="$OUT/${h}__${flat: -90}"
  [ -e "$dest" ] && { echo "skip $f"; continue; }
  if [ $((total + size)) -gt "$CAP" ]; then echo "cap reached, skipping $f"; continue; fi
  if cp -X "$f" "$dest" 2>/dev/null && [ "$(stat -f %z "$dest")" = "$size" ]; then
    total=$((total + size)); echo "copy $f ($size)"
  else
    rm -f "$dest"; echo "FAIL $f"
  fi
done
echo "done: $(ls "$OUT" | wc -l | tr -d ' ') files, $(du -sk "$OUT" | cut -f1) KB"

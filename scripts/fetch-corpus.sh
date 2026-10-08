#!/bin/bash
# Download public Wavefront OBJ files into corpus/. Idempotent; failures are skipped.
#   corpus/mcguire/  McGuire Computer Graphics Archive (scenes and scans, several exporters)
#   corpus/c3d/      alecjacobson/common-3d-test-models (scanned meshes)
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAILED=()

# fetch_zip SET NAME URL [PART...]: every *.obj member of the zip (split parts first) goes to
# corpus/SET/NAME[-member].obj
fetch_zip() {
  local set=$1 name=$2 url=$3; shift 3
  local out="$ROOT/corpus/$set" tmp m dest n
  mkdir -p "$out"
  ls "$out/$name".obj "$out/$name"-*.obj >/dev/null 2>&1 && { echo "skip  $set/$name"; return; }
  tmp=$(mktemp -d "$ROOT/corpus/.dl.XXXXXX")
  local ok=1 zip="$tmp/a.zip"
  curl -fsSL --retry 2 -m 1800 -o "$zip" "$url" || ok=0
  if [ $ok = 1 ] && [ $# -gt 0 ]; then
    for p in "$@"; do curl -fsSL --retry 2 -m 1800 -o "$tmp/a.${p##*.}" "$p" || ok=0; done
    [ $ok = 1 ] && { zip -q -s 0 "$zip" --out "$tmp/full.zip" || ok=0; zip="$tmp/full.zip"; }
  fi
  if [ $ok = 1 ]; then
    mkdir "$tmp/x"
    n=$(unzip -Z1 "$zip" | grep -ic '\.obj$')
    unzip -Z1 "$zip" | grep -i '\.obj$' | grep -v '^__MACOSX/' | while IFS= read -r m; do
      unzip -q -o "$zip" "$m" -d "$tmp/x" || continue
      if [ "$n" = 1 ]; then dest="$out/$name.obj"
      else b=$(basename "$m"); dest="$out/$name-${b%.*}.obj"; fi
      mv "$tmp/x/$m" "$dest"; echo "ok    ${dest#$ROOT/} ($(wc -c < "$dest") bytes)"
    done
    [ "$n" -gt 0 ] || ok=0
  fi
  [ $ok = 1 ] || { echo "FAIL  $set/$name <- $url"; FAILED+=("$set/$name"); }
  rm -rf "$tmp"
}

# fetch_obj SET NAME URL: a plain .obj file
fetch_obj() {
  local set=$1 name=$2 url=$3 out="$ROOT/corpus/$1" tmp
  mkdir -p "$out"
  [ -s "$out/$name.obj" ] && { echo "skip  $set/$name"; return; }
  tmp=$(mktemp -d "$ROOT/corpus/.dl.XXXXXX")
  if curl -fsSL --retry 2 -m 1800 -o "$tmp/f" "$url"; then
    mv "$tmp/f" "$out/$name.obj"; echo "ok    $set/$name.obj ($(wc -c < "$out/$name.obj") bytes)"
  else
    echo "FAIL  $set/$name <- $url"; FAILED+=("$set/$name")
  fi
  rm -rf "$tmp"
}

CE=https://casual-effects.com/g3d/data10
for m in hairball San_Miguel rungholt buddha powerplant dragon lpshead bmw gallery vokselia_spawn \
         living_room sibenik conference fireplace_room breakfast_room salle_de_bain erato bedroom; do
  fetch_zip mcguire "$m" "$CE/research/model/$m/$m.zip"
done
fetch_zip mcguire crytek_sponza "$CE/common/model/crytek_sponza/sponza.zip"

C3D=https://raw.githubusercontent.com/alecjacobson/common-3d-test-models/master/data
# (The large zips there hold the original PLY scans.)
for m in xyzrgb_dragon igea bimba horse lucy happy armadillo max-planck ogre nefertiti beast \
         stanford-bunny beetle-alt rocker-arm cheburashka fandisk homer; do
  fetch_obj c3d "$m" "$C3D/$m.obj"
done

echo "failed: ${FAILED[*]:-none}"

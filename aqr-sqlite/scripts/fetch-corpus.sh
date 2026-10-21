#!/bin/bash
# Download public real-world SQLite DBs into corpus/public/. Idempotent; failures are skipped.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/corpus/public"
mkdir -p "$OUT"
MAX=157286400   # 150 MB cap per file
FAILED=()

is_sqlite() { [ "$(head -c 16 "$1" | od -An -c | tr -d ' \n')" = 'SQLiteformat3\0' ]; }

# fetch NAME URL  -> plain database file
fetch() {
  local name=$1 url=$2 dest="$OUT/$1" tmp
  [ -s "$dest" ] && { echo "skip  $name"; return; }
  tmp=$(mktemp -d "$ROOT/corpus/.dl.XXXXXX")
  if curl -fsSL --retry 2 -m 600 --max-filesize $MAX -o "$tmp/f" "$url" && is_sqlite "$tmp/f"; then
    mv "$tmp/f" "$dest"; echo "ok    $name ($(wc -c < "$dest") bytes)"
  else
    echo "FAIL  $name <- $url"; FAILED+=("$name")
  fi
  rm -rf "$tmp"
}

# fetch_zip NAME URL MEMBER -> extract MEMBER from zip
fetch_zip() {
  local name=$1 url=$2 member=$3 dest="$OUT/$1" tmp
  [ -s "$dest" ] && { echo "skip  $name"; return; }
  tmp=$(mktemp -d "$ROOT/corpus/.dl.XXXXXX")
  if curl -fsSL --retry 2 -m 600 --max-filesize $MAX -o "$tmp/a.zip" "$url" \
     && mkdir "$tmp/x" && unzip -q -o "$tmp/a.zip" "$member" -d "$tmp/x" && is_sqlite "$tmp/x/$member"; then
    mv "$tmp/x/$member" "$dest"; echo "ok    $name ($(wc -c < "$dest") bytes)"
  else
    echo "FAIL  $name <- $url"; FAILED+=("$name")
  fi
  rm -rf "$tmp"
}

GH=https://raw.githubusercontent.com
fetch chinook.sqlite            https://github.com/lerocha/chinook-database/releases/latest/download/Chinook_Sqlite.sqlite
fetch chinook-autoinc.sqlite    https://github.com/lerocha/chinook-database/releases/latest/download/Chinook_Sqlite_AutoIncrementPKs.sqlite
fetch_zip chinook-tutorial.db   https://www.sqlitetutorial.net/wp-content/uploads/2018/03/chinook.zip chinook.db
fetch northwind.db              $GH/jpwhite3/northwind-SQLite3/main/dist/northwind.db
fetch sakila.db                 $GH/ivanceras/sakila/master/sqlite-sakila-db/sakila.db
fetch sqlite-jdbc-sample.db     $GH/xerial/sqlite-jdbc/master/src/test/resources/org/sqlite/sample.db
fetch datasette-fixtures.db     https://latest.datasette.io/fixtures.db
fetch datasette-content.db      https://datasette.io/content.db
fetch til.db                    https://til.simonwillison.net/tils.db
# Skipped: regmem.db and global-power-plants (>150 MB cap); legislators/fivethirtyeight datasettes URLs 404/503.
fetch rivers.gpkg               https://www.geopackage.org/data/rivers.gpkg
fetch gdal_sample.gpkg          https://www.geopackage.org/data/gdal_sample.gpkg
fetch plain_1.mbtiles           $GH/mapbox/node-mbtiles/master/test/fixtures/plain_1.mbtiles
fetch one_tile.mbtiles          https://github.com/mapbox/mbutil/raw/master/test/data/one_tile.mbtiles

echo "failed: ${FAILED[*]:-none}"

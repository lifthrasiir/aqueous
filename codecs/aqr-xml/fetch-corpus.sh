#!/bin/sh
# Downloads aqr-xml's samples into corpus/ (README.md, "Corpus"); a sample there is not fetched
# again. Needs curl, gzip and 7zz (7-Zip).
set -eu
cd "$(dirname "$0")"
mkdir -p corpus
C=$PWD/corpus

# fetch NAME URL...: each URL into corpus/NAME/, unpacked by its extension
fetch() {
    name=$1
    shift
    [ -e "$C/$name/.done" ] && return
    rm -rf "${C:?}/$name"
    mkdir -p "$C/$name"
    for url in "$@"; do
        f=$C/$name/$(basename "$url")
        echo "$name: $url"
        curl -fL --retry 3 -o "$f" "$url"
        case $f in
        *.7z) (cd "$C/$name" && 7zz x -y -bd "$f" >/dev/null) && rm "$f" ;;
        *.gz) gzip -d "$f" ;;
        esac
    done
    touch "$C/$name/.done"
}

fetch se-coffee https://archive.org/download/stackexchange/coffee.stackexchange.com.7z
fetch se-history https://archive.org/download/stackexchange/history.stackexchange.com.7z
# (hours 124503 and 124504, 2026-11-25 21:00 to 23:00 UTC)
fetch osm-hours \
    https://planet.openstreetmap.org/replication/hour/000/124/503.osc.gz \
    https://planet.openstreetmap.org/replication/hour/000/124/504.osc.gz
fetch pubmed https://ftp.ncbi.nlm.nih.gov/pubmed/baseline/pubmed26n0001.xml.gz
fetch jmdict http://ftp.edrdg.org/pub/Nihongo/JMdict_e.gz
U=https://aiweb.cs.washington.edu/research/projects/xmltk/xmldata/data
fetch uw $U/treebank/treebank_e.xml.gz $U/SwissProt/SwissProt.xml.gz $U/tpc-h/lineitem.xml.gz \
    $U/nasa/nasa.xml.gz

# DBLP's first 256 MiB (of 1.1 GB compressed), up to the end of its last whole line
if [ ! -e "$C/dblp/.done" ]; then
    mkdir -p "$C/dblp"
    echo "dblp: https://dblp.org/xml/dblp.xml.gz (256 MiB of it)"
    { curl -sfL https://dblp.org/xml/dblp.xml.gz || true; } | { gzip -dc 2>/dev/null || true; } |
        head -c 268435456 >"$C/dblp/dblp.part"
    perl -e 'open F, "+<", $ARGV[0] or die; local $/; $d = <F>; $d =~ s/[^\n]*\z//; seek F, 0, 0; print F $d; truncate F, length $d' "$C/dblp/dblp.part"
    mv "$C/dblp/dblp.part" "$C/dblp/dblp-head.xml"
    touch "$C/dblp/.done"
fi
du -sh "$C"/*

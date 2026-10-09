#!/bin/sh
# Fetches the public corpus into corpus/ (not tracked):
#   cc/     1,000 PDF files from Common Crawl (SafeDocs CC-MAIN-2021-31-PDF-UNTRUNCATED,
#           digitalcorpora.org, zip 0000), mixed producers.
#   arxiv/  arXiv papers (pdfTeX, LuaTeX, ...), from scripts/arxiv-ids.txt.
set -eu
cd "$(dirname "$0")/.."
mkdir -p corpus/dl corpus/cc corpus/arxiv
Z=corpus/dl/cc-0000.zip
URL=https://digitalcorpora.s3.amazonaws.com/corpora/files/CC-MAIN-2021-31-PDF-UNTRUNCATED/zipfiles/0000-0999/0000.zip
if [ ! -s "$Z" ]; then
    # Resumed until complete: the server drops long transfers.
    for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        curl -fsSL -C - -o "$Z.part" "$URL" && break
        sleep 2
    done
    mv "$Z.part" "$Z"
fi
[ -n "$(ls corpus/cc)" ] || unzip -q -j -d corpus/cc "$Z"
while read -r id; do
    [ -s "corpus/arxiv/$id.pdf" ] || curl -fsSL -o "corpus/arxiv/$id.pdf" "https://arxiv.org/pdf/$id" || true
    sleep 1
done < scripts/arxiv-ids.txt
du -sh corpus/cc corpus/arxiv

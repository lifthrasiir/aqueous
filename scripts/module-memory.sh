#!/bin/zsh
# Measures the working memory of the modules (aqr-decompress's example aqr-decompress-memory) on
# a sample of each kind of input: the samples made or copied once into
# crates/aqr-decompress/cache/memory/samples, compressed once with aqr -c (.aqr beside them), and
# the modules run as WebAssembly with the tasks on 1 and 8 threads (results-t1.txt, results-t8.txt).
# Usage: scripts/module-memory.sh [sample...]: the samples (the names below; all by default)
# compressed where they are not yet, and then every sample compressed measured.
set -eu
cd "${0:A:h}/.."
root=$PWD
dir=$root/crates/aqr-decompress/cache/memory
s=$dir/samples
mkdir -p $s
data=/Volumes/Data
codecs=$root/codecs

cargo build -q --release -p aqr -p aqr-decompress --example aqr-decompress-memory
aqr=$root/target/release/aqr
memory=$root/target/release/examples/aqr-decompress-memory

# a sample `name` made by the shell command `cmd` writing $out, once
make() {
    local out=$s/$1
    [[ -e $out ]] && return
    eval "$2" || { rm -rf $out; echo "module-memory.sh: $1 not made" >&2; }
}
copy() { local src=$2; make $1 'cp "$src" $out'; }

py() { python3 -I -c "$1" "$2"; }

samples() {
    # text of 12 MB: the system's word lists and the workspace's sources
    make text.txt '{ cat /usr/share/dict/words /usr/share/dict/web2a; git ls-files -z "*.rs" "*.md" "*.bs" | xargs -0 cat; } | head -c 12000000 > $out'
    local t=$s/text.txt
    make text.gz 'gzip -1c $t > $out'
    # (lzma pays where its data is archived as well, and only at high levels: see `high`)
    make text.lzma 'lzma -6c $t > $out'
    make lzma.d 'mkdir $out && cp $t $s/text.lzma $out'
    make text.lz4 'lz4 -1c $t > $out'
    make text.lzo 'lzop -c $t > $out'
    make src.tar 'git ls-files -z codecs/aqr-flate | xargs -0 tar cf $out'
    copy zip.zip $codecs/aqr-zip/corpus/881026.zip
    copy git.pack $codecs/aqr-git/corpus/local/aqueous/.git/objects/pack/pack-d45a1b85c3d4567983098805e604a5afab43e22e.pack
    copy photo.png $codecs/aqr-png/corpus/codec-corpus/clic2025/training/5dbdb989cf026f7ab3dd7167d7fc0fe2.png
    copy anim.png $codecs/aqr-png/corpus/codec-corpus/image-rs/test-images/png/apng/ball.png
    make flat.png 'magick -size 8000x6000 gradient:white-gray90 -depth 8 $out'
    make photo.jpg 'magick $s/photo.png -quality 90 $out'
    make photo-prog.jpg 'magick $s/photo.png -quality 90 -interlace JPEG $out'
    make big.jpg 'magick $s/photo.png -resize 4000x3000! -quality 90 $out'
    make photo.bmp 'magick $s/photo.png -type TrueColor BMP3:$out'
    make photo16.tif 'magick $s/photo.png -depth 16 -compress none $out'
    copy tex.dds $data/crew-tex/common/0536a3dc348cb382.dds
    copy layers.psd $data/aqr-psd-corpus/genposter/2565695.psd
    copy art.clip "$data/aqr-clip-corpus/AimeReform_maimai.clip"
    copy song.mp3 $data/aqr-mp3-corpus/fma/fma_small/006/006779.mp3
    make tone.wav 'sox -n -r 44100 -b 16 -c 2 $out synth 300 pinknoise vol 0.3'
    make tone.mp3 'lame --quiet -b 128 $s/tone.wav $out'
    copy live.flac $data/aqr-flac-corpus/hires/deadandcompany.2018-06-30.dpa4022.mixpre6.flac24__2018.06.30.S01t01_Tuning.flac
    copy win.dll $codecs/aqr-exe/corpus/sample/C/Windows/System32/mfc140ud.dll
    copy net.dll "$codecs/aqr-exe/corpus/sample/C/Program Files/dotnet/shared/Microsoft.NETCore.App/2.0.6/System.Private.Xml.dll"
    copy mac.bin /usr/bin/vim
    copy paper.pdf $codecs/aqr-pdf/corpus/arxiv/2307.09288.pdf
    copy db.sqlite $codecs/aqr-sqlite/corpus/system/f502b9__ystem_Library_Security_Certificates.bundle_Contents_Resources_ValidProduction_valid.sqlite
    copy image.fits $codecs/aqr-fits/corpus/misc/frame-r-001000-1-0027.fits
    copy image.fits.fz $codecs/aqr-fits/corpus/misc/legacysurvey-0001m002-image-g.fits.fz
    copy mesh.obj $data/aqr-obj-corpus/c3d/bimba.obj
    make table.csv 'py "
import random, sys
r = random.Random(1)
with open(sys.argv[1], \"w\") as f:
    f.write(\"id,name,score,ratio,date\\n\")
    for i in range(200000):
        f.write(f\"{i},user{r.randrange(5000)},{r.randrange(1000)},{r.random():.4f},2026-{r.randrange(1,13):02}-{r.randrange(1,29):02}\\n\")
" $out'
    make genome.fa 'py "
import random, sys
r = random.Random(1)
with open(sys.argv[1], \"w\") as f:
    for c in range(4):
        f.write(f\">chr{c} synthetic\\n\")
        s = \"\".join(r.choice(\"ACGT\") for _ in range(2000000))
        for i in range(0, len(s), 60):
            f.write(s[i:i+60] + \"\\n\")
" $out'
    make records.bin 'py "
import random, struct, sys
r = random.Random(1)
with open(sys.argv[1], \"wb\") as f:
    t = 1700000000
    for i in range(400000):
        t += r.randrange(30)
        f.write(struct.pack(\"<IIhhf\", i, t, r.randrange(-500, 500), r.randrange(100), r.random()))
" $out'
    make messages.po 'py "
import random, sys
r = random.Random(1)
w = open(\"/usr/share/dict/words\").read().split()
with open(sys.argv[1], \"w\") as f:
    f.write(\"msgid \\\"\\\"\\nmsgstr \\\"Content-Type: text/plain; charset=UTF-8\\\\n\\\"\\n\\n\")
    for i in range(20000):
        a = \" \".join(r.choice(w) for _ in range(6))
        f.write(f\"msgid \\\"{i} {a}\\\"\\nmsgstr \\\"{a[::-1]}\\\"\\n\\n\")
" $out'
    make messages.mo 'msgfmt -o $out $s/messages.po'
}

samples
names=("$@")
(( $#names )) || names=($(cd $s && ls | rg -v '\.(aqr|po)$'))
# samples whose recompressors the lower levels do not try
high=(lzma.d)
for n in $names; do
    [[ -f $s/$n.aqr ]] && continue
    level=()
    (( ${high[(Ie)$n]} )) && level=(-l 9)
    $aqr -c -q $level -o $s/$n.aqr $s/$n || echo "module-memory.sh: $n not compressed" >&2
done
# (every sample compressed so far)
for t in 1 8; do
    $memory -t $t $s/*.aqr(N) | tee $dir/results-t$t.txt
done

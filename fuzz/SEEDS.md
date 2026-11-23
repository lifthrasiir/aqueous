# Licences of the seeds and corpora

This covers the seeds `run.sh` writes (`src/bin/seeds.rs`) and the corpora under `corpus/` that
the fuzzers grow from them. libFuzzer mutates its inputs and crosses them, so any file of a
corpus may be derived from one or more seeds. Its files are named by hash and carry no
attribution of their own. A corpus is therefore distributed under all of the terms below
together, with this file and `licenses/` beside it.

The texts named below are in `licenses/`. The NOTICE files of committed fixtures are
in the crates' `tests/data`. Copy them into a published corpus as well.

## 1. The project's own: CC0-1.0

The following are dedicated to the public domain under CC0 1.0 (`licenses/CC0-1.0.txt`):

- the seeds `seeds.rs` makes: archives of each instruction and of the toy modules, trees, sealed
  and damaged archives, settings records, bundles of synthetic blobs (`src/sfx.rs`), and the
  inputs `src/unpack.rs`, `src/pack.rs` and `src/modules.rs` make;
- the synthetic data of `src/seedgen.rs`, and what the tools it runs (sqlite3, ffmpeg, msgfmt, cc,
  xz) make of that data alone;
- the codecs' and crates' committed test files (`codecs/*/tests/data`, `crates/*/tests/data`,
  `crates/aqr-archive/testdata`), except the ones in section 2;
- the Wasm modules of aqr-modules (`crates/aqr-modules/wasm`), except the code of others linked
  into them (section 3).

## 2. Committed fixtures of other projects

| Files | Source | Licence | Notice |
|---|---|---|---|
| `codecs/aqr-egg/tests/data` (EGG archives) | akolman/EggDotNet's test files | MIT | `codecs/aqr-egg/tests/data/NOTICE.md` |
| `codecs/aqr-lha/tests/data` (LHA archives) | fragglet/lhasa's `test/archives` | ISC | `codecs/aqr-lha/tests/data/NOTICE.md` |
| `codecs/aqr-gif/tests/data` (made of ImageMagick's built-in images) | ImageMagick | ImageMagick | `codecs/aqr-gif/tests/data/NOTICE.md` |
| `codecs/aqr-gif/tests/data/dup-box2.gif` | Apache's icon `box2.gif` | public domain | the same |
| `codecs/aqr-alz/tests/data`, `codecs/aqr-bz2/tests/data/alz` | alkegi/unalz's `tests/data` | CC0-1.0 | their `README.md` |

## 3. Code of others linked into the Wasm modules

The modules are both seeds of `modules` and seeds of `compress`. They contain the following:

| Code | Licence (as chosen here, of the crate's offer) | Text |
|---|---|---|
| Rust's `core` and `alloc` | Apache-2.0 (of MIT OR Apache-2.0) | `Apache-2.0.txt` |
| block-buffer 0.10.4, cfg-if 1.0.5, crypto-common 0.1.7, digest 0.10.7, sha1 0.10.7, sha2 0.10.9, typenum 1.20.1 | Apache-2.0 (of MIT OR Apache-2.0) | `Apache-2.0.txt` |
| generic-array 0.14.7 | MIT | `generic-array.txt` |
| zmij 1.0.23 | MIT | `zmij.txt` |
| memchr 2.8.3 | Unlicense (of Unlicense OR MIT) | none needed |
| xxhash-rust 0.8.19 | BSL-1.0 (its notice is not required of object code) | none needed |

## 4. Local corpora of other projects

These are the files of the codecs' local corpora (`corpus/`, not committed) that `seeds.rs`'s
`local()` takes: files of at most 256 KiB, a spread of 30 from the arsc and dwarf sets. They are
seeds of `compress` and of `natives` (and so of `wasm`).

### PSD files (aqr-psd)

| Files | Source | Licence | Text |
|---|---|---|---|
| 67 of `tests/psd_files` (not `third-party-psds/`) | psd-tools/psd-tools | MIT | `psd-tools.txt` |
| 11 of `Tests/images` | python-pillow/Pillow | MIT-CMU | `Pillow.txt` |
| 29 of `packages/psd` | webtoon/psd | MIT | `webtoon-psd.txt` |

Neither project states a licence for its test files apart from the repository's own.

### Binary XML of Android apps (aqr-arsc)

These are 30 resource files of the F-Droid builds below, as aapt2 compiled them. Some are the
resources of the libraries the apps bundle: Material Components (`res/color/mtrl_*`, `m3_*`,
`design_*`) and AndroidX. Those libraries are under Apache-2.0, by the Android Open Source
Project.

| App (version code) | Version | Source (commit) | Licence | Text |
|---|---|---|---|---|
| Organic Maps, `app.organicmaps` (26092932) | 2026.09.29-32-FDroid | organicmaps/organicmaps `b20d1417eef1` | Apache-2.0 | `Apache-2.0.txt`, `organicmaps-NOTICE.txt` |
| K-9 Mail, `com.fsck.k9` (39046) | 24.0 | thunderbird/thunderbird-android `73dd96a435d2` | Apache-2.0 | `Apache-2.0.txt`, `k9-NOTICE.txt` |
| AntennaPod, `de.danoeh.antennapod` (3120395) | 3.12.3 | AntennaPod/AntennaPod `175e5ef8fe3e` | GPL-3.0-only | `GPL-3.0.txt` |
| F-Droid, `org.fdroid.fdroid` (1023052) | 1.23.2 | gitlab.com/fdroid/fdroidclient `0ea06b1c1d68` | GPL-3.0-or-later | `GPL-3.0.txt` |
| Fennec F-Droid, `org.mozilla.fennec_fdroid` (1590020) | 159.0.0 | gitlab.com/relan/fennecbuild `232bf44ba83d` (Mozilla's Firefox for Android) | MPL-2.0 | `MPL-2.0.txt` |
| NewPipe, `org.schabi.newpipe` (1015) | 0.29.1 | TeamNewPipe/NewPipe `00acf2f318b0` | GPL-3.0-or-later | `GPL-3.0.txt` |
| VLC, `org.videolan.vlc` (13070208) | 3.7.2 | code.videolan.org/videolan/vlc-android `e5740f713d99` | GPL-2.0-or-later (its `COPYING`; F-Droid states GPL-3.0-only for the app as built) | `GPL-2.0.txt`, `GPL-3.0.txt` |

Organic Maps' NOTICE asks for a mention of the "Organic Maps Project" with a link to
https://organicmaps.app; this is it. None of these seeds are Organic Maps' `.mwm` map data
(`DATA_LICENSE.txt`).

### Debugging information (aqr-dwarf)

30 of the 273 files of `/usr/lib/debug/.build-id/` of Ubuntu 24.04's **libc6-dbg
2.39-0ubuntu8.9, arm64** (source package glibc). They were checked byte for byte against
https://launchpad.net/ubuntu/+archive/primary/+files/libc6-dbg_2.39-0ubuntu8.9_arm64.deb.
They are under the GNU C Library's licences: mostly LGPL-2.1-or-later, with the parts and the
other terms that the package's `copyright` lists (`glibc-ubuntu-copyright.txt`, `LGPL-2.1.txt`,
`GPL-2.0.txt`).

### SQLite databases (aqr-sqlite)

These are the 4 files of the public set of at most 256 KiB. They were fetched by
`codecs/aqr-sqlite/scripts/fetch-corpus.sh`.

| File | Source | Licence | Text |
|---|---|---|---|
| `datasette-fixtures.db` | https://latest.datasette.io/fixtures.db (simonw/datasette's test fixtures) | Apache-2.0 | `Apache-2.0.txt` |
| `sqlite-jdbc-sample.db` | xerial/sqlite-jdbc, `src/test/resources/org/sqlite/sample.db` | Apache-2.0 | `Apache-2.0.txt` |
| `one_tile.mbtiles` | mapbox/mbutil, `test/data/one_tile.mbtiles` | BSD-3-Clause | `mbutil.txt` |
| `gdal_sample.gpkg` | https://www.geopackage.org/data/gdal_sample.gpkg (OGC's GeoPackage site) | none stated; not copyrightable (below) | — |

`gdal_sample.gpkg` comes with no terms, but it holds nothing that copyright covers. It is the
output GDAL wrote on 2014-08-27 to test every geometry type. It has 16 tables of 2 rows each (5
to 8 for the collections), of a few vertices of small numbers (the line string is `(1 2, 3 4)`);
the only other values are `1`, `'foo'`, `1.23456`, a date and `007FFF`. The rest is the schema the GeoPackage specification fixes, the standard
definitions of EPSG:4326 and EPSG:32631, and R-tree indexes.

## 5. Source of the copyleft parts

The GPL, LGPL and MPL parts of section 4 are object code. Their corresponding source is at:

- the F-Droid apps: `https://f-droid.org/repo/<appid>_<versioncode>_src.tar.gz`, for each row of
  the table (all 7 there as of 2026-11-23). Fennec's tarball is of its build scripts, which fetch
  Mozilla's tree.
- glibc: https://launchpad.net/ubuntu/+source/glibc/2.39-0ubuntu8.9

GPL-3.0 §6(d) lets a distributor point to source on a third party's server, but the distributor
then has to make sure it stays available. A published corpus is safer with copies of these
sources beside it.

## 6. Open points

- Pillow's test images, psd-tools' `issues/` and webtoon/psd's fixtures are covered only by
  their repositories' licences; their own provenance is not stated.
- The fuzzer may cross several sources into one input. No source here is GPL-2.0-only: VLC and
  glibc's GPL parts are GPL-2.0-or-later, glibc's LGPL-2.1-or-later, and Mozilla does not mark
  its code "Incompatible With Secondary Licenses" (not checked file by file). So such an input
  can always be distributed under GPL-3.0-or-later, with the notices of everything it may come
  from (all of the above). The ImageMagick License has not been reviewed by the FSF for GPL-3.0
  compatibility; it is modelled on Apache-2.0, which is compatible.

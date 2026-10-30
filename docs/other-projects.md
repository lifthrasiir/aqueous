# 다른 프로젝트 조사

encode.su 포럼에서 Aqueous와 겹치는 프로젝트를 모으고, 저장소가 있는 것은 코드를 읽어 기법과 시사점을 정리한다.
목록은 2026-10-24에 Wayback Machine에 남은 스레드 제목 3,844개(전체 약 4,560개 중 84%)에서 골랐으므로 빠진 것이 있을 수 있다.
encode.su에만 첨부된 파일은 받지 않았고, 공개 저장소(GitHub 등)나 공식 배포처가 있는 것만 읽었다.

항목의 상태 표기는 다음과 같다.

- **[분석]** 저장소의 코드를 읽고 정리했다.
- **[개요]** 문서나 스레드 본문으로만 정리했다(코드 없음, 비공개, 또는 encode.su 첨부뿐).
- **[미조사]** 목록에만 올렸다.

중요도는 **A**(같은 문제를 푸는 직접 경쟁·참고 구현), **B**(특정 크레이트의 설계에 쓸 만한 것), **C**(배경 지식)로 나눈다.
스레드 번호는 `https://encode.su/threads/<번호>`이다.

분석은 프로젝트별로 코드를 읽은 결과이다. 빌드하거나 돌려 보지는 않았으므로, 남의 수치는 모두 그쪽 주장이다.

## 요약: 먼저 검토할 것

2026-10-30의 코드베이스 기준이다.

목표:

- 10gb는 지금 2.584 GB이고, 주 목표는 2.4 GB 이하, Insighter는 2.27 GB이다. 크기 예산과 남은 일은 11절과 `10gb-todo.md`에 있다.

명세와 설계 문서:

- 블록 헤더가 손상된 뒤 다음 leaf block에서 재동기화하는 절차를 Repair 절(recovery-design S3)에 넣는다(9절).
- 적합성 수준마다 보장하는 메모리 한도의 하한이 없다(명세 `#module-execution`의 Issue). 지금은 아카이브가 적용마다 선언한 한도(`apply_memory_limit`)와 구현이 정한 한도뿐이다(9절).

크레이트:

- aqr-compress: 헤더 없는 raw deflate를 찾지 않는다. Precomp `-brute`의 거르개가 참고가 된다(2절).
- aqr-flate: 프로파일 9(1.0 s, 1.07%)와 10(6.2 s, 0.51%) 사이가 비어 있고, 그 자리에 preflate-rs(1.7 s, 0.78%)가 있다. 후처리기(deft4j류)가 만든 스트림의 비율을 잰다(2절).
- aqr-mp3: packMP3와의 격차 가운데 남은 후보는 같은 granule 이웃 문맥과 부호 모델이다(5절).
- aqr-vorbis: FSB5 안의 패킷만 받는다. Ogg 페이지를 재구성하면 `.ogg`가 열린다(5절).
- aqr-lz4: HC와 legacy frame을 넣으면 커널, initramfs, squashfs가 열린다(3절).
- aqr-raw: PCM 경로에서 샘플 도메인 LMS 캐스케이드와 Sac식 편향 보정을 시험한다. 래스터는 이미지별 색 변환 선택을 시험한다(5절, 6절).
- aqr-flac: 가변 블록 크기 표본으로 시험한다(flacoutcpp, flake)(5절).
- aqr-exe: ELF 지원, RISC-V(xz의 `AUIPC` 쌍에서 출발), ARM64의 `ADRP`와 lo12 묶기, PIC 점프 표, 점프 타깃의 델타 부호화 비교(8절).
- aqr-pdf: 빈 사용자 암호의 암호화 파일은 공개 도구 중 하는 것이 없다(7절).

새 포맷 후보: ALAC, CD 이미지(ECM식), zstd 레벨 1–4, LZF, GIF(인코더 프로파일과 정정), BC7 텍스처.

---

## 1. 범용 재압축기(precompressor)

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| Precomp | 47, 53, 57, 133, 307, 690, 1799, 2411, 2416, 2497, 2511, 2828, 3076, 3223 외 | A | [분석] |
| xtool | 4000 | A | [분석] |
| AntiZ | 2197 | A | [분석] |
| Superduper Precompressor | 2605 | C | [개요] |
| Fairytale (아카이버, 중단) | 1846, 2927 | A | [분석] |
| Insighter | 4541 | A | [개요] |

## 2. DEFLATE 재현

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| preflate, preflate-rs | (수집 목록에 없음) | A | [분석] |
| reflate | 1399 | A | [개요] |
| Zflate | 3998 | C | [개요] |
| ZREC | 3990 | B | [개요] |
| zRecompress | 1590 | C | [개요] |
| grittibanzli | (xtool에서 언급) | B | [분석] |
| rezop | 2204 | B | [개요] |
| defdb | 1428 | C | [개요] |
| "deflate recompressor" (Shelwien, 1401), 재귀 inflate (3738), deflate 은닉 (1406) | 1401, 3738, 1406 | C | [개요] |
| DEFLATE 인코더 모음: zlib 구현 목록, libdeflate, zlib-ng, Intel deflate, kzip, Zopfli, ECT, turtledeflate, Columbo, defluff, DeflOpt | 352, 358, 2482, 2219, 2203, 1630, 2274, 4479, 4518, 1214, 455 | B | [분석] |

## 3. 다른 LZ 계열 재현

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| LZX recompression PoC | 1252 | B | [분석] |
| lzma recompressor, LZMA recompression | 1231, 1642 | B | [분석] |
| LZMA 설정 역추적 | 4187 | C | [분석] |
| LZ4 동점 처리와 버전별 출력 | 3097, 3328 | B | [분석] |
| CAB/LZX, WIM | 589, 2133, 2665, 146 | C | [분석] |
| LZO, LZF, GDeflate, Oodle, zstd | (xtool 등에서 언급) | B | [분석] |

## 4. JPEG

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| packJPG (멀티스레드판 포함) | 852, 1412, 2448, 4482 외 | A | [분석] |
| Lepton | 2552 | A | [분석] |
| brunsli | 3128 | A | [분석] |
| paq8px JPEG 모델 | 342, 921 | A | [분석] |
| GDCC 2023 과제 1 (JPEG 무손실 재압축) | 4048, 4059 | A | [개요] |
| packARC | 1557, 1839, 1861 | C | [개요] |
| UPGRADE_PAQJPVG, jojpeg | 4453, 2785 | C | [개요] |
| StuffIt JPEG, WinZip 12 JPEG | 865, 166 | C | [개요] |

## 5. 오디오

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| packMP3 | 1549, 1807 | A | [분석] |
| mp3dump, MP3 repacker | 271, 932 | C | [개요] |
| balrogg (Vorbis/Opus) | 4545 | A | [분석] |
| oggre, ogg_rehuff, rehuff, OptiVorbis | 3256 | B | [개요] |
| FLACOut | 1986 | B | [개요] |
| OptimFROG, TAK, TTA, Sac, Newpack, hac | 1219, 3612, 780, 1137, 3531, 4180 | B | [분석] |
| Monkey's Audio, WavPack, ALAC | (README) | B | [분석] |

## 6. 이미지

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| Precomp의 PNG/GIF 처리 | 47, 53 | B | [분석] |
| flexiGIF | 3008 | B | [개요] |
| CryoPNG | 1260 | C | [개요] |
| packPNM | 1394 | B | [개요] |
| 무손실 이미지 코덱(GraLIC, BMF, FLIF, QOI, HALIC 등) | 595, 2321, 3753, 4025 | C | [분석] |
| 카메라 RAW: Rawzor, PackRAW, rawdet | 1261, 1775, 1998, 2762, 3383 | B | [개요] |
| PSD, TIFF | 338, 552, 1308, 2671, 332 | C | [개요] |
| 텍스처: DXT 재압축기, crunch, BCPack | 2901, 1579, 3562 | C | [분석] |

## 7. 문서·비디오·디스크 이미지

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| PDF: lossless PDF compressor, smpdf | 3313, 2022, 2551 | B | [분석] |
| 비디오: H.264 재압축, MJPEG 중복 제거, Bink | 2285, 1466, 3716, 2100 | C | [개요] |
| ECM, YBE (CD 이미지) | 504, 2090, 4061 | B | [분석] |

## 8. 실행 파일

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| disasm 기반 필터 (Shelwien) | 557 | B | [개요] |
| Courgette | 412 | B | [분석] |
| x86 split-stream, BCJ2 | 1930, 1945 | B | [분석] |
| ARM, ARM64, RISC-V 필터 | 2025, 3905, 4084 | B | [분석] |
| ELF (재배치 포맷, HCR) | 3858, 3976, 4266 | B | [분석] |
| kkrunchy disfilter, Crinkler, paq8px ExeModel, .NET 압축기 | 1911, 402 | B | [분석] |

## 9. 아카이브 포맷과 VM

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| ZPAQ, ZPAQL, zpaqfranz | 281, 256, 1273, 2185, 4369, 4387, 3840 | A | [분석] |
| paq8pxv (VM) | 3064 | B | [분석] |
| RarVM | 1618 | B | [분석] |
| OpenZL | 4439 | A | [분석] |
| 압축기의 Wasm 컴파일 | 3374, 3864 | C | [개요] |
| 포맷 설계 토론: Universal Archive Format, 장기 보관, 재현 가능 아카이브, 압축기 파일 포맷 | 115, 2087, 3391, 2631, 2670, 4445 | C | [분석] |
| SYC | 4480 | C | [개요] |

## 10. 중복 제거와 구조화 데이터

| 프로젝트 | 스레드 | 중요도 | 상태 |
|---|---|---|---|
| srep, eXdupe | 583, 2839, 1735, 4142 | C | [개요] |
| BtrBlocks, 부동소수점·수치 테이블 | 4146, 1318, 4067, 4472 | C | [분석] |
| Draco, 3D 메시 | 2696, 2324, 2117 | C | [분석] |

---

# 분석

## 1. 범용 재압축기

### xtool [분석]

- https://github.com/Razor12911/xtool, MIT. 플러그인은 https://github.com/Razor12911/xtool-plugins. 마지막 릴리스 ES_R54(0.7.9), 마지막 푸시 2023-09. Delphi, 코어 `precompressor/*.pas` 약 12.8k줄이고 외부 코덱은 모두 DLL로 부른다. 게임 리패커들이 쓴다.
- 코덱(각 `Precomp*.pas` 앞의 이름 배열):
  - DEFLATE(`PrecompZLib.pas`): `zlib`은 실제 zlib으로 레벨 1–9 × memLevel 1–7 × windowBits를 탐색한다. 레벨 1, 6, 9 대표값으로 묶어 탐색 폭을 줄인다. `reflate`와 `preflate`(grittibanzli 포함)는 차이 정보 방식이다. `png`는 IDAT를 이어 붙여 넘긴다.
  - LZ4, LZ4HC, LZ4F, LZO(1x, 2a, 1c), zstd, gdeflate(DirectStorage).
  - Oodle(kraken, mermaid, selkie, hydra, leviathan, lzna): **게임이 가져온 Oodle DLL로 `OodleLZ_Compress`를 다시 부른다.**
  - 미디어(flac, packjpg, brunsli, jojpeg), 암호(xor, aes, rc4. 암호화된 안쪽을 더 풀기 위한 것).
- 재현의 일반 규칙: 원래 라이브러리로 다시 압축해 바이트 비교하고, 다르면 zstd 기반 패치를 만들어 패치/스트림이 `DIFF_TOLERANCE`(기본 5%) 이하일 때만 받는다(`PrecompUtils.pas`).
- 구조:
  - `Scan1`(후보), `Scan2`(확인), `Process`(재압축 검증), `EncData`(기록)이다. 코덱 인터페이스는 함수 포인터 레코드 `TPrecompressor`이고, 호스트가 할당, 출력, 해시, 패치, 저장소 함수 표를 넘긴다.
  - 깊이 1–10까지 풀린 결과를 다음 깊이에 넣는다. 플러그인도 자식 스트림을 선언할 수 있다.
  - 스트림 헤더는 `Kind`(기본, 확장, 중첩, 중복), 크기 둘, 리소스, 코덱 번호, 옵션 비트필드이다.
  - 중복 제거: xxh3_128과 크기로 같은 스트림을 찾고, 외부 srep에도 파이프한다. 출력은 풀린 스트림과 헤더가 섞인 바이트열이며, 아카이브 컨테이너가 아니다.
  - **스트림 DB**(`TDatabase`, `dbgenerator/`): 체크섬에서 성공한 코덱과 옵션으로 가는 표이다. 맞으면 파라미터 탐색을 건너뛴다.
  - 게임 엔진 컨테이너(Unreal, Unity, Frostbite 등)는 INI 플러그인(`PrecompINI.pas`)으로 **선언적으로** 서술한다. 필드의 오프셋, 크기, 식(`ParseExpression`), 조건을 쓴다. DLL 플러그인(C ABI)과 EXE 플러그인(stdio)도 있다.
- 디코더 로직은 도구에 고정이다. 스트림에는 코덱 번호와 옵션만 있어서, 같은 xtool 빌드와 같은 외부 DLL이 있어야 풀린다. Windows 전용이다.

### AntiZ [분석]

- https://github.com/hxim/AntiZ, GPL-3.0. 2015-06에서 멈췄다. C++ `main.cpp` 하나(1,731줄).
- zlib 헤더 4종(0x78 01/5E/9C/DA)만 찾는다. inflate로 확인한 뒤 `deflateInit2`의 레벨 × window × memLevel을 전수 시도하고, 다른 바이트가 128개 이하면 (오프셋 증분, 값) 목록으로 저장한다. 크기 차이가 128 B를 넘으면 비교하지 않는다.
- 출력은 ATZ1 파일(스트림마다 위치, 길이, 파라미터 3바이트, diff 목록)이다. 재귀, 병렬, 중복 제거는 없다.

### Fairytale [분석]

- https://github.com/schnaader/fairytale, LGPL-3.0. C++, 커뮤니티 아카이버 시제품. 2018-03–2018-09 사이 30커밋(schnaader, andrew-epstein, Márcio Pais 등) 뒤로 멈췄다. README가 스스로 "압축은 아직 하지 않는 대충의 시제품"이라 한다.
- 구현된 것:
  - 분석기(`analyser.cpp`): strict 파서 다음 fuzzy 파서 순서로 깊이 4까지 돈다. 파서는 DEFLATE(일반, brute), JPEG, BMP, DDS, MOD, TEXT, JSON이다.
  - 블록 트리와 전 레벨 중복 제거(`Deduper`).
  - 변환은 paq8px에서 가져온 zlib 변환 하나뿐이다. 헤더 24종, 레벨 × memLevel 병렬 재압축, 최대 64개의 penalty byte.
  - 저장소 관리(`HybridStream`, `StorageManager`): 메모리와 파일 사이를 오가고 우선순위와 참조 수를 둔다. 예산을 넘으면 중간 스트림을 버렸다가 필요할 때 부모 변환을 다시 돌려 되살린다(`attemptRevival`).
- 포맷 초안(Márcio Pais, "Fairytale File Format Draft v1.2", 2018, gitter 첨부 PDF. 구현되지 않음):
  - 파일 헤더, 압축 데이터, 코덱 정의, 블록 분할, 디렉터리 트리, 파일 목록 순서이다. 뒤 구조들은 모두 크기(VLI) + 내용 + CRC32이고, 메타데이터는 모르는 태그를 무시하는 태그 목록이다.
  - 청크마다 "코덱 시퀀스" 번호를 참조한다. 중첩 변환 트리를 블록 id 참조의 평면 표로 적는다. 블록마다 2바이트 체크섬을 두어 기존 아카이브에 추가할 때 빠르게 중복을 판정한다.
  - 디코더 로직은 아카이브에 실리지 않는다.
- 멈춘 이유는 문서로 찾지 못했다. 이력만 보면 분석 계층과 포맷 초안까지 가고 압축기, 기록기, 복원기가 이어지지 않았다.

### Insighter [개요]

- 스레드 4541(Gonzalo, 2026-08-30–10-26). 2026-10-24의 수집 목록 뒤에 올라왔다. 소스는 비공개이고 Zig로 썼다. 공개 계획은 있지만 시점은 정하지 않았다. 아래 수치는 모두 스레드의 주장이다.
- 구조:
  - 파서가 먼저이다. 컨테이너(TAR, ISO, ZIP, CAB, CFBF, AR, PDF, CHM, WinHelp)를 열고 데이터 카버로 안쪽 스트림을 찾는다. 깊이는 7까지 간다.
  - 파일 단위와 블록 단위의 전역 중복 제거를 한다. 같은 JPEG가 파일로, Office 문서 안에, 그 문서를 담은 ZIP 안에 있으면 한 번만 처리한다.
  - 전처리한 데이터를 종류별 버킷에 모은다. 버킷마다 REP식 장거리 중복 제거(Bulat의 REP를 단순화)나 xwrt식 텍스트 전처리를 거친 뒤, 정해진 청크 크기마다 FastLZMA2, PPMd 또는 무압축으로 보낸다. PPMd는 빼는 중이다.
  - 압축할 수 없는 데이터도 REP는 거치고 마지막 압축만 건너뛴다.
  - 풀 때 임시 파일이 꼭 필요하다. 압축은 전부 메모리에서 하고, 메모리를 아끼는 것보다 압축률과 속도를 우선한다고 밝혔다.
- 재압축과 변환:
  - DEFLATE(preflate), JPEG(libjxl. 산술 부호화 JPEG는 packJPG), 선택 사항인 solid JPEG(DCT 계수를 묶음, Zig 자체 구현), PCM(FLAC), bzip2(어떤 도구가 만든 스트림이든 된다고 주장), LZW(GIF, TIFF, PDF), SZDD(`*.??_`), MSZIP을 다시 압축한다. LZX는 작업 중이다.
  - x86/x64 필터 "Cacciavite"(dispack류, PE와 ELF), DNA의 2비트 packing과 비트맵 마스크, 이미지류 데이터의 stride 인지 델타, AoS→SoA 전치, Base64 복원도 한다.
  - 모르는 데이터에는 적응형 변환을 대충 대입해 본다. 이 가운데 ADPT 변환은 데이터를 너무 바꿔 뒤단의 중복 제거를 망쳤기 때문에 기본에서 껐다.
  - DNA는 2026-10-26에 REP + 2비트 + flzma2에서 DNA 전용 LZMA류로 바꿨다. 다른 7.3 GB 데이터에서 크기 −7%, 압축 속도 2배, 메모리 1/3이라고 한다. CM은 "몇 %p 더 줄지만 자원이 50배"라고 했다.
  - LZMA 재현은 비현실적이라고 본다. WavPack은 FLAC과 어느 쪽이 나은지 미리 알 방법이 없어 넣지 않았다.
- 수치 (M4 MacBook):
  - 10gb는 2,271,616,726 B(22.7%)로 줄인다. 압축은 42.6 MB/s, 메모리는 12.7 GB이다.
  - 풀기는 2.2 GB였던 시점에 81.3 MB/s(2.0분)였다.
  - 스레드의 "9.3 GB"는 10^10 B를 GiB로 적은 것이다.
  - enwik9는 FastLZMA2 경로로 209.9 MB, PPMd 경로로 197.8 MB이다. 비교용 LTCB의 PPMd는 184.0 MB, fxz 최상 옵션은 202.5 MB이다.
- 10gb에서의 통계(2.2 GB 시점):
  - 컨테이너: ZIP 781개(항목 46,087개, 625 MB), TAR 16개(163 MB), AR 297개(멤버 11,028개, 139 MB)
  - 데이터 종류: DNA 3.0 GB, 텍스트 2.0 GB, PE 1.1 GB, DEFLATE 스트림 664 MB, Incompressible 633 MB, JPEG 273 MB, COFF 오브젝트 106 MB, BMP 67 MB, 반복 바이트 64 MB, WAV 52 MB
  - 처리한 파일 79,438개 가운데 중복이 15,911개이다.

### Aqueous에 대한 시사점

- **파라미터 DB**(xtool): 스트림 지문에서 성공한 파라미터를 기억해 탐색을 건너뛴다. 포맷에는 영향이 없는 인코더 최적화로, 같은 도구가 만든 PDF·ZIP·게임 에셋에서 압축 시간을 줄일 수 있다. aqr-flate가 이미 "앞 스트림의 파라미터 재사용"을 하므로 그것을 실행 간에 유지하는 형태가 된다.
- **선언적 컨테이너 서술**(xtool INI): 엔진별 컨테이너를 코드 없이 적는다. Aqueous는 컨테이너마다 Rust 크레이트를 짜야 한다. 게임 에셋을 대상으로 삼는다면 작은 기술 언어가 필요해진다(디코드 쪽은 어차피 `slice`와 `concat`이라 인코더 전용이다).
- **게임 코덱 공백**: Oodle(Kraken, Mermaid, Selkie, Leviathan)과 gdeflate. xtool은 Oodle을 DLL 재호출로 풀 뿐이라 결정적 Wasm으로 옮길 수 없다. Aqueous에서는 클린룸 구현이 아니면 불가능하다. gdeflate는 swizzle을 풀면 aqr-flate가 받는다(3절).
- **패치 허용 임계**(xtool 5%, AntiZ 128 B, Fairytale 64 B): 정확히 재현하지 못하면 패치 크기로 채택을 정한다. aqr-flate, aqr-lzma, aqr-lzo는 레코드가 있어 필요 없다. aqr-lz4나 aqr-snappy처럼 "재현 못 하면 그대로 둔다"인 크레이트에는 일반 패치 경로가 하나의 선택지이다.
- **Fairytale의 revival**: 메모리 예산을 넘는 중간 스트림을 버리고 부모에서 다시 만드는 방식은 압축기의 메모리 관리와 견줄 만하다. 블록별 부분 체크섬(증분 추가용 중복 판정)도 Aqueous에 없다.
- Aqueous가 앞선 점: 디코더가 아카이브 안의 결정적 Wasm이라 도구, DLL, zlib 버전에 묶이지 않는다. 포맷은 완결된 아카이브(엔트리, 식, 무결성)이다. 이 그룹은 모두 외부 zlib 바이너리의 동작에 기대므로, zlib 버전이 바뀌면 재현이 깨질 수 있다.


## 2. DEFLATE 재현

### Precomp [분석]

- https://github.com/schnaader/precomp-cpp, Apache-2.0. 마지막 커밋은 2025-06(LICENSE 갱신)이고 문서상 0.4.7–0.4.8이다. C++, 본체는 `precomp.cpp` 하나(8.5k줄)이다. `contrib/`에 zlib 1.2.11, preflate 0.3.5, packJPG, packMP3, brunsli, brotli, bzip2, giflib, liblzma가 소스째 들어 있다.
- DEFLATE는 자체 모델 없이 전부 preflate에 맡긴다(`try_recompression_deflate()`, 3160행 부근). 메타블록은 기본 2 MiB(`-pfmeta`)이고, `-pfverify`는 즉석 왕복 검증이다.
  - 옛 방식(zlib 레벨 × memLevel × windowBits를 시도하고 "penalty bytes"로 차이를 고친다)은 `penalty_bytes` 같은 전역 변수로만 남아 있다. 정적으로 읽은 판단이다.
- 재구성 정보는 그대로 PCF에 쓰고 전체를 LZMA2나 bzip2로 압축한다. 풀어낸 평문을 다시 검사하는 재귀가 있다.
- 스트림 탐지(`compress_file()`, 3676–4560행 부근):
  - ZIP 로컬 헤더, gzip(FLG 예약 비트와 선택 필드 파싱), PDF `/FlateDecode`, PNG `IDAT`(여러 청크를 이어 붙이고 CRC와 길이를 따로 저장), GIF, JPEG, MP3, SWF `CWS`, base64, bzip2의 시그니처를 차례로 본다.
  - `-intense`는 모든 위치에서 zlib 헤더 2바이트를 본다.
  - `-brute`는 모든 위치에서 raw deflate를 시도한다(`check_inf_result`, 2712행). BTYPE=3과 stored를 버리고, 64바이트 ×4 구간의 바이트 히스토그램으로 중복이 심한 위치를 거르고, zlib `inflate`를 2048 B만 돌려 끝났다면 1024 B 이상 풀린 것만 받는다(일반 모드는 32 B). 실패한 위치는 기록해 다시 보지 않는다.
- 공개 수치는 README의 silesia.zip 예시 하나이다(7z -mx9 99.7%, precomp 69.7%).

### preflate [분석]

- https://github.com/deus-libri/preflate, Apache-2.0, Dirk Steinke. 0.3.5(2018-09)에서 멈췄다. C++ 약 7.2k줄. raw DEFLATE만 다루며 컨테이너 파서는 없다.
- 구조는 토큰화, 파라미터 추정, 토큰별 예측과 오예측 기록, 정적 모델과 산술 부호화이다.
  - 추정(`preflate_parameter_estimator.cpp`, `preflate_complevel_estimator.cpp`): windowBits는 최대 거리에서, memLevel은 블록당 최대 토큰 수에서 구한다. 레벨은 fast(1–3)와 slow 해시 체인을 모두 돌려 체인 깊이, 먼 길이 3 매치 등을 재서 정한다. 인코더 모델은 zlib 하나이다.
  - 토큰 예측(`preflate_token_predictor.cpp`): zlib식 lazy/greedy 탐색을 다시 돌린다. 리터럴/참조 오예측은 이진으로, 길이는 예측과의 차이로, 거리는 해시 체인 홉 수로 기록한다. 길이 258의 비정규 부호화도 따로 기록한다.
  - 블록 경계는 EOB 오예측 비트와 블록 종류로, 허프만 트리(`preflate_tree_predictor.cpp`)는 빈도에서 zlib식 트리를 다시 만들고 심볼 수, 코드 길이 코드, 반복 종류와 횟수, 비트 길이의 차이를 고친다.
- 정정 스트림: 메타블록마다 통계를 모아, 서브모델이 기본값이면 모델을 보내지 않고 아니면 양자화한 정적 모델을 앞에 쓴 뒤 산술 부호화한다. 2패스, 비적응이다.
- 0.3.0부터 메타블록 단위로 병렬 처리한다. 쪼갠 구간에서 레벨 추정이 실패해 정정이 커지는 문제가 0.3.1에서 기록됐다.
- 수치는 서술뿐이다. zlib 스트림의 20–30%는 정정이 3 B를 넘고 보통 수십 B이다. 단일 스레드로 reflate나 옛 Precomp보다 50–500% 느리다. grittibanzli의 정정이 20–30% 크다고도 한다(모두 저자 주장).

### preflate-rs [분석]

- https://github.com/microsoft/preflate-rs, Apache-2.0. 활발하다(2026-10-23 커밋). Rust 2024, `#![forbid(unsafe_code)]`. 핵심 `preflate/`가 약 9.1k줄, 컨테이너 `container/`가 약 3.6k줄이고 `dll/`(.NET FFI)과 `fuzz/`가 있다. 구조가 C++ preflate와 대응하지만 포트라는 명시는 확인하지 못했다.
- 추정(`estimator/`): `TokenPredictorParameters`는 windowBits, nice_length, max_chain, min_len, strategy, add policy, greedy/lazy(good, max_lazy), max_dist_3_matches, max_token_count, 블록 종류 전략, 해시 알고리즘을 담는다.
  - 해시 후보(`hash_algorithm.rs`, `complevel_estimator.rs`): zlib(memLevel별 세 가지), MiniZFast, Libdeflate4(4바이트와 3바이트 보조), Libdeflate4Fast, ZlibNG, Crc32c, RandomVector. 후보마다 체인 깊이를 재서 가장 작은 것을 고르고, 4096을 넘으면 `NoCompressionCandidates`로 거부한다.
  - `add_policy_estimator.rs`: 매치 안의 위치를 해시에 넣는 규칙을 추정한다. AddAll, AddFirst(n), AddFirstAndLast(n), miniz fast의 4K 경계, zlib-ng fast의 32K 경계.
- 예측(`token_predictor.rs`): 체인 탐색, 먼 길이 3 매치 규칙, good_length에서 체인 줄이기, lazy. 정정은 리터럴/참조 오예측, 길이 차이(지그재그), 거리는 홉 수이다. 블록 경계는 `max_token_count`로 예측하고 어긋나면 `TokenCount`를 기록한다. 거리가 틀리면 정답 길이로 다시 예측하고, 그래도 안 되면 체인 4096으로 재시도한다.
- 트리(`tree_predictor.rs`): zlib과 miniz_oxide 두 비트 길이 계산기만 시도한다. libdeflate와 zopfli용은 없다.
- 정정 부호화(`cabac_codec.rs`): 정정 17종마다 문맥 두 개(기본값 연속 길이, 정정값)를 둔 VP8식 CABAC(lepton_jpeg_rust와 공유)이다. 위치나 앞의 오예측 같은 문맥은 쓰지 않는다.
- 컨테이너(`container/src/`): LITERAL, DEFLATE, PNG, DEFLATE_CONTINUE 블록, 블록별 zstd, JPEG는 lepton_jpeg. 탐지(`scan_deflate.rs`)는 zlib 헤더, ZIP 로컬 헤더, gzip, IHDR/IDAT, JPEG 마커만 보고 헤더 없는 raw deflate는 찾지 않는다. 평문 1024 B 미만은 리터럴로 둔다.
- README 수치(평문 대비 정정): zlib 0–9가 0.01–0.08%, zlib-ng가 레벨 4–6에서 0.90–1.07%(9는 미지원), libdeflate 0.01–1.51%, miniz_oxide 0.01–2.70%.

### reflate [개요]

- 공개 소스를 찾지 못했다. PowerArchiver에 들어 있다고 한다.
- preflate README의 서술: 어떤 DEFLATE든 재구성하지만(7-Zip, kzip 포함) zlib 레벨 9에서만 거의 완벽하고, 레벨이 낮을수록 재구성 정보가 커진다. 같은 저자의 rawdet는 raw deflate를 찾는 도구이다.


### grittibanzli [분석]

- https://github.com/google/grittibanzli, Apache-2.0. 2018-04에서 멈췄다. `grittibanzli.cc` 하나(1,591줄).
- DEFLATE를 원본 데이터, "deflate choices", 컨테이너 메타의 바이트 스트림으로 나눈다. 엔트로피 부호화는 직접 하지 않고, 0이 많은 choices 스트림을 Brotli나 PPMd로 압축하라고 한다.
- 예측기는 15비트 해시(`kHashShift=5`)와 `chain[32768]`, 그리고 0 런 전용 체인이다. 예측과 다른 토큰만 기록한다. 해시에 의존하지 않는 정확 체인이라는 점에서 aqr-flate의 chain 규칙과 같은 접근이다.
- 길이 258을 284+31로 쓴 스트림, 스트림 뒤 쓰레기, 32비트를 넘는 크기는 받지 않는다. aqr-flate는 모두 받는다.
- README 수치: gzip -6–9에서 0.5–1%, zopfli 9%, gzip -1 21%.

### ECT, turtledeflate, deft4j [분석]

- ECT(https://github.com/fhanau/Efficient-Compression-Tool, Apache-2.0, 활발):
  - `src/zopfli/`가 고친 Zopfli이고, `lz77.c`에 LZ4HC 파생 매치 탐색기가 있다.
  - 이미 flate-zoo의 `ect` 백엔드(레벨 1–9)이고, aqr-flate가 헤더 빌더를 갖고 있다.
- turtledeflate(https://github.com/rwillenbacher/turtledeflate, BSD-2-Clause, 2026-03):
  - kzip에서 영감을 받은 매우 느린 압축기이다. 다단계 블록 분할과 약간 무작위화한 고정소수점 경로 탐색을 쓴다.
  - README로는 Zopfli와 ECT보다 0.1–3% 작다.
  - 무작위화 때문에 에뮬레이션은 불가능하다. 블록 경계가 최적화된 스트림이라, chain 규칙의 상한을 시험할 표본으로만 쓸모가 있다.
- deft4j(https://github.com/NeRdTheNed/deft4j, 0BSD, 2023-11, WIP):
  - Java 구현의 DEFLATE 후처리기이다. ZIP, gzip, zlib, PNG/APNG를 받는다.
  - 하는 일은 역참조를 리터럴로 바꾸기, 동적 헤더 재부호화, 블록 종류를 2→1→0으로 바꾸기, 빈 블록 제거, 코드 길이 끝의 0 제거이다.
  - JarTighten 같은 JAR와 게임 에셋 최적화기가 이런 출력을 낸다. 토큰은 원래 인코더의 것이고 헤더와 블록만 바뀐다.

### rezop, ZREC, Zflate, zRecompress, defdb, kzip, DeflOpt, defluff, Columbo [개요]

- rezop, ZREC, zRecompress, defdb: 공개 저장소를 찾지 못했다.
  - ZREC는 스레드 3990 본문상 preflate 0.3.5를 쓰는 시행착오식 zlib 재압축기이다.
  - Zflate(3998)는 reflate의 출력을 zpaq으로 압축한 것이고, 소스는 encode.su 첨부뿐이다.
- kzip, PNGOUT: Ken Silverman의 닫힌 바이너리이다. 최적 파싱과 블록 탐색을 쓰고, PNG와 게임 에셋 ZIP에서 만난다.
- DeflOpt, defluff, Columbo: 닫혀 있거나 코드를 구하지 못했다. 토큰을 두고 헤더와 블록만 다시 쓰는 후처리기로 알려져 있다(미검증).

### flate-zoo에 없는 인코더

flate-zoo에는 zlib 1.3.1, zlib-ng, Chromium(CRC 해시 포함), Cloudflare, zlib-rs, libdeflate, Zopfli(C, Rust), miniz, miniz-oxide, yazi, deflate-rs, libflate, fdeflate, stb, lodepng, sdefl, uzlib, libslz, Go(std, klauspost), gzip, Info-ZIP, ISA-L(C 경로), 7-Zip, advdef, Crypto++, ECT, fflate, pako, uzip, zlibjs, Apple libcompression이 이미 있다. 그 밖의 것:

| 인코더 | 출력이 나오는 곳 | 에뮬레이션 | 우선순위 |
|---|---|---|---|
| deft4j, JarTighten 같은 후처리기 | JAR, Minecraft 에셋, PNG | 토큰은 원래 인코더, 블록과 헤더만 변형 | 중간 |
| DeflOpt, defluff, Columbo | 옛 PNG·ZIP 최적화 체인 | 위와 같음(미검증) | 중간 |
| Intel zlib 포크(jtkukunas/zlib) | 일부 Linux 배포판과 서버 | zlib에 CRC32 해시(Chromium과 같은 꼴, 미검증) | 중간, Chromium 작업과 묶음 |
| ISA-L igzip의 asm 경로 | 스토리지, Linux 도구 | C 경로와 다를 수 있음(flate-zoo README 경고) | 중간, 실제 빌드 표본 필요 |
| kzip, PNGOUT | PNG, 게임 ZIP | 불가(최적 파싱) | 낮음, chain 규칙으로 |
| turtledeflate | 거의 없음 | 불가(무작위화) | 낮음, 상한 시험용 |
| Zig, .NET/Java, Nim, OCaml 구현 | 각 언어 런타임 | 대부분 zlib 계열 | 낮음 |

### Aqueous에 대한 시사점(인코더 생태계)

- 공개 인코더는 거의 모두 flate-zoo에 있다. 모자란 것은 표본이 아니라 에뮬레이터이고, 순서는 aqr-flate "Next steps" 그대로가 맞다. zlib-ng `deflate_medium`, Chromium·Cloudflare·Intel의 CRC 해시(구현을 공유할 수 있다), libdeflate 순이다.
- **후처리기 산물이 숨은 비용일 수 있다.** 역참조의 리터럴 치환, 블록 종류 변경, 빈 블록, 코드 길이 끝의 0 제거가 섞이면 토큰도 헤더도 에뮬레이션과 어긋난다. PNG와 JAR 표본에서 헤더 불일치와 블록 종류 이상의 비율을 재 보고, 많다면 이런 변형을 헤더 후보에 넣는다.
- 최적 파싱 인코더(kzip, PNGOUT, turtledeflate)는 chain 규칙과 순위 부호화가 한계이다. 공개 재구현이 없으니 다른 방법도 없다.


### Aqueous에 대한 시사점

aqr-flate README의 홀드아웃 벤치(공통으로 왕복하는 807개 스트림, 40.6 MB, M4 한 스레드)가 기준이다.

| 도구 | 정정 | 실패 | 분할 | 결합 |
|---|---|---|---|---|
| aqr-flate 프로파일 10 | 0.512% | 0 | 6.2 s | 1.6 s |
| aqr-flate 프로파일 9 | 1.069% | 0 | 1.0 s | 0.8 s |
| aqr-flate 프로파일 6 | 1.153% | 0 | 1.0 s | 0.7 s |
| preflate | 2.378% | 15 | 3.8 s | 3.0 s |
| preflate-rs | 0.780% | 117 | 1.7 s | 0.8 s |

- 크기는 프로파일 10이 앞서지만 속도는 그렇지 않다. preflate-rs는 0.780%에 분할 1.7 s이다. aqr-flate가 그보다 빠른 프로파일 9(1.0 s)에서는 정정이 1.4배 크다. 그 사이 영역(예: 2–3 s에 0.6–0.7%)은 비어 있다.
- preflate-rs는 자기가 정확히 모델링하는 인코더(gzip, miniz-oxide, sdefl, zlib-cloudflare, zlib-ng)에서 더 작다(aqr-flate README). 그 해시와 add policy 목록(4K·32K 경계 규칙, Crc32c, Libdeflate4Fast)은 aqr-flate "다음 단계"의 zlib-ng, miniz, Chromium/Cloudflare 에뮬레이션에 바로 참고가 된다. Apache-2.0이므로 귀속을 지키면 코드 참고도 된다.
- preflate-rs의 13% 거부는 대부분 체인 4096 한계(`NoCompressionCandidates`)이다. 7-Zip, zopfli 같은 출력은 aqr-flate의 체인 규칙과 순위 부호화가 유리한 영역이다.
- **헤더 없는 raw deflate 탐지가 aqr-compress에 없다.** 지금은 컨테이너(gzip, zlib, ZIP, PNG, PDF)를 거쳐서만 찾는다. Precomp `-brute`의 거르개(BTYPE, 바이트 히스토그램, 2 KiB 부분 inflate와 최소 출력, 실패 위치 기록)는 그대로 비용 모델이 된다. 게임 아카이브나 독자 컨테이너에 쓸모가 있다.
- 블록 레이아웃: aqr-flate의 zlib 정정 5.3 KB는 대부분 작은 memLevel의 stored 블록 길이이다. preflate 계열도 이를 예측하지 않고 정정만 하므로 배울 것이 없다. zlib의 `_tr_flush_block` 결정을 에뮬레이션하는 쪽이 낫다.
- 정정 부호화는 aqr-flate(lookahead 문맥과 prior)가 preflate 계열(종류별 문맥 하나, 또는 스트림별 정적 모델)보다 정교하다. 허프만 헤더도 aqr-flate는 zlib, libdeflate, zopfli/ECT 빌더를 갖추고 preflate-rs는 zlib과 miniz뿐이다.


## 3. 다른 LZ 계열과 게임 코덱

### LZMA, LZMA2, xz [분석]

- liblzma(https://github.com/tukaani-project/xz, 0BSD):
  - 2012년 이후 인코더 핵심 파일(`src/liblzma/lzma/lzma_encoder*.c`, `lzma_encoder_optimum_{normal,fast}.c`, `lzma2_encoder.c`, `lz/lz_encoder_mf.c`)의 이력을 보면, 출력을 바꾼 변경은 두 개뿐이다.
    - 2015-02 `117d9626`: fast 모드 압축률 회귀 수정
    - 2022-11 `3be88ae0`: nice_len 2–3 허용
  - 나머지는 리팩터링, 경고, SPDX 헤더, `lzma_memcmplen()` 도입이다. `lzma_memcmplen`이 출력을 바꾸지 않았는지는 확인하지 못했다.
  - 그렇다면 5.x의 normal 모드는 사실상 한 가족이고, 변수는 버전이 아니라 설정이다. 설정은 preset과 `-e`, dict, lc/lp/pb, 매치 탐색기, depth, nice_len, 그리고 `-T`의 블록 분할(기본 블록은 dict의 3배)이다.
- 7-Zip(LZMA SDK): liblzma와 가격 모델, 매치 탐색기, 파서가 달라 출력이 다르다.
  - lzma-sdk-rs(https://github.com/danifunker/lzma-sdk-rs, BSD-3-Clause)는 LZMA SDK 23.01 단일 스레드 인코더의 Rust 포트이다. `LzmaEnc_MemEncode`와 바이트가 같다고 주장한다(docs.rs 요약만 읽었다).
  - 7-Zip 버전 사이의 인코더 변화는 확인하지 못했다.
- 7-Zip-zstd(https://github.com/mcmilk/7-Zip-zstd): LZMA2 레벨 1–9를 Fast LZMA2로 바꾸므로 같은 `.7z`라도 출력이 다르다.
- LZMA 재압축기의 공개 사례는 찾지 못했다.
- 2026-10-27: 10gb의 mingw `*.tar.lzma` 123개(2011년)는 XZ Utils 5.8.4의 `xz --format=lzma -6`으로 다시 압축하면 모두 바이트가 같다. 위의 "5.x는 한 가족"을 뒷받침한다. Aqueous는 그래도 인코더를 재현하지 않고 parse를 레코드로 적어 재구성한다(aqr-lzma, 레코드는 스트림의 13–18%, 11절).

### LZX, XPRESS, LZMS [분석]

- wimlib(https://wimlib.net, LGPL-3.0+ 또는 GPL-3.0+):
  - 파일은 `src/lzx_compress.c`(약 2.9k줄), `lzms_compress.c`, `xpress_compress.c`이다.
  - LZX는 레벨에 따라 lazy와 근사 최적(반복 패스 수가 레벨과 함께 는다) 두 알고리즘이 있다. CAB의 sliding window 모드는 없다.
  - 포럼에서 저자가 Microsoft 구현과 호환되지만 바이트가 같지는 않다고 했다. 따라서 wimlib는 Microsoft 인코더의 재현이 아니다.
- Microsoft 인코더(`cabinet.dll`, `wimgapi.dll`, Compression API)는 닫혀 있다.
  - ms-compress(https://github.com/coderforlife/ms-compress)는 LZNT1, XPRESS, XPRESS Huffman이 동작하고 LZX는 빠졌다. 원본과 대조하지 않았고 압축률이 "약간 낫다"고 한다. 라이선스는 GPL로 보인다.
  - libmspack과 go-winio의 LZX는 디코더뿐이다. LZX 재압축 PoC의 공개 코드는 찾지 못했다.
- CAB의 LZX는 프레임 사이에 32 KiB 상태를 이어 가므로, 폴더 하나를 한 스트림으로 다뤄야 한다.

### LZ4 버전과 HC [분석]

- `lz4/NEWS`:
  - 출력이 바뀐 것으로 읽히는 버전은 1.7.3(작은 파일 압축률), 1.8.1(레벨 10 이상), 1.8.2(아주 작은 압축률 개선), 1.10.0(레벨 2 신설, HC 3–9 약 5%)이다.
  - 1.9.x에는 압축률 변경 언급이 없다.
  - 1.10.0은 `nbWorkers`와 무관하게 같은 출력을 내고, `lz4`와 `lz4-nomt`의 프레임도 같다.
- 따라서 fast는 1.8.2 이후 한 가족일 가능성이 높고, aqr-lz4의 "1.10.0 하나로 전부 재현"과 맞는다. 1.7.x 이전을 번들한 앱은 다른 가족이다(fixture로 확인 필요).
- HC: 레벨 3–9는 해시 체인, 10–12는 최적 파서이고, 레벨별 파라미터는 고정 표이다. 같은 `lz4hc.c` 한 계보라 aqr-lz4처럼 포팅할 수 있다.
- HC가 나오는 곳: Linux 커널 이미지와 initramfs(legacy frame, 8 MiB 블록, 레벨 미확인), squashfs와 EROFS, Unity AssetBundle LZ4HC, Android 부트 이미지. legacy frame과 HC는 aqr-lz4가 지금 그대로 두는 것들이다.

### LZO, LZF [분석]

- LZO(https://www.oberhumer.com/opensource/lzo/, GPL, 상용 라이선스 별도):
  - miniLZO는 LZO1X-1 압축기와 해제기만 담고, LZO1X-999(최적 파서, 레벨 1–9)는 따로이다.
  - 커널 `lib/lzo/lzo1x_compress.c`는 miniLZO에서 왔다. 5.1의 LZO-RLE는 0 런을 RLE로 쓰는 다른 비트스트림이고, 34 KiB 이상 입력의 모호성 버그 때문에 압축기만 고쳐진 적이 있다. 커널판과 miniLZO의 출력이 같은지는 확인하지 못했다.
  - 쓰이는 곳: zram, UBIFS, JFFS2, btrfs, 커널 이미지, 임베디드 펌웨어, Procreate, 옛 게임.
  - 2026-10-28: aqr-lzo가 압축기를 재현하지 않고 데이터와 parse의 레코드로 나눈다(변종이 많고 GPL이라). LZO1X-1의 거리는 99.9%가 "앞 매치 안이 아닌 위치 중 그 길이의 가장 가까운 것"이라 레코드가 스트림의 3.5–4.6%, LZO1X-999는 2–6%이다(aqr-lzo README).
- LZF(liblzf, Marc Lehmann, Redis 번들 `lzf_c.c`, 약 300줄):
  - `VERY_FAST`, `ULTRA_FAST`, `HLOG`, `STRICT_ALIGN`, `INIT_HTAB` 매크로가 출력을 바꾼다. 번들마다 설정이 달라 후보 가족을 차례로 시도해야 한다.
  - 쓰이는 곳: Redis RDB, Krita, Perl `Compress::LZF`.

### GDeflate [분석]

- https://github.com/microsoft/DirectStorage 의 `GDeflate/`(MIT). 압축기(`3rdparty/libdeflate/lib/gdeflate_compress.c`, libdeflate 포크)와 CPU·HLSL 해제기가 있다.
- 입력을 64 KiB 타일로 나누고, 타일마다 DEFLATE 비트스트림의 심볼을 32개 하위 스트림에 번갈아 배치한다(swizzle). 규칙은 README에 완결되어 있다.
- swizzle은 순열이므로 풀어서 보통의 DEFLATE로 만들면 aqr-flate가 받는다. 압축기는 libdeflate 레벨을 그대로 쓴다.
- 쓰는 게임이 아직 적다(DirectStorage 1.1 이후의 일부).

### Oodle [개요]

- RAD(현재 Epic Games Tools)의 상용 라이브러리이다.
- 공개 디코더: ooz(https://github.com/powzix/ooz, Kraken, Mermaid, Selkie, Leviathan, LZNA, BitKnit. 라이선스 파일을 찾지 못했고 GPL 우려가 언급된다), oozle(GPL-3.0+ 래퍼). ooz의 압축 옵션은 공식 DLL을 부를 뿐이다.
- 공개된 인코더 재현은 찾지 못했다.
- 쓰는 곳: Unreal Engine 4.2x 이후와 5의 `.pak`과 IoStore, 다수의 AAA 게임. 암호화나 서명이 얹히는 경우가 많다.

### zstd [분석]

- https://github.com/facebook/zstd(BSD 또는 GPL-2.0). `CHANGELOG`에 따르면 거의 매 버전 압축률이 바뀐다.
  - 1.4.x는 dfast(레벨 3, 4) 개선, 1.5.0은 높은 레벨에서 블록 분할기를 기본으로 켰고, 1.5.1–1.5.2는 레벨 재조정, 1.5.5는 높은 레벨의 압축률 개선, 최신 판은 다시 dfast와 블록 경계를 고쳤다.
  - 그러니 기본 레벨 3의 출력도 버전마다 다르다.
- 반면 `lib/zstd.h`의 `ZSTD_isDeterministicBuild()`가 **같은 버전을 표준 옵션으로 빌드하면 빌드끼리 출력이 같다**고 보증한다.
- 실제 사례: OpenZFS의 zstd 1.5.2→1.5.7 업그레이드로 MAC이 어긋났다(FreeBSD 278099). DNF delta RPM이 zstd 버전 차이로 체크섬에 실패했다(LWN 922043).
- 압축기를 바이트 같게 포팅한 공개 구현은 찾지 못했다.

### 우선순위

| 코덱 | 나오는 곳 | 재현 난이도 | 공개 코드 | 우선순위 |
|---|---|---|---|---|
| LZ4 HC, legacy frame, linked block | 커널, initramfs, squashfs, EROFS, Unity | 낮음–중간(`lz4hc.c` 포팅) | liblz4(BSD-2) | 높음 |
| zstd 레벨 1–4(1.5.x 한두 판) | 패키지, 커널, HTTP, OCI, 게임 | 중간(버전별 분기) | libzstd | 높음 |
| LZ4 옛 가족(1.8.1 이하) | 옛 번들 | 낮음(fixture 추가) | liblz4 이력 | 중간 |
| LZF | Redis, Krita | 낮음 | liblzf | 중간 |
| LZO1X 스트림(lzop 밖), LZO-RLE | 커널, squashfs, UBIFS, zram, Procreate | aqr-lzo의 레코드를 다른 컨테이너로(lzop 파일은 완료) | 커널 `lzo.rst`, liblzo2를 블랙박스로 | 중간 |
| LZMA2(xz), 7z 컨테이너와 BCJ2 | 패키지, 7z, OTA | aqr-lzma의 레코드(LZMA1과 `.lzma`는 완료) | xz(0BSD), lzma-sdk-rs | 중간 |
| LZX(CAB, WIM, CHM) | Windows 설치·업데이트 | 높음(닫힌 인코더) | wimlib(다른 인코더) | 중간(파싱 기반) |
| GDeflate | DirectStorage 에셋 | 낮음(swizzle만) | MS(MIT) | 낮음 |
| zstd 레벨 5 이상, 멀티스레드 | 패키지 | 높음 | libzstd | 낮음 |
| LZMS, XPRESS Huffman | ESD, 프리페치 | 높음 | wimlib, ms-compress | 낮음 |
| Oodle | AAA `.pak` | 매우 높음 | 디코더뿐 | 낮음(재현), 파싱 기반은 검토 |

### Aqueous에 대한 시사점

- **aqr-lz4의 다음 걸음은 HC와 legacy frame이다.** 같은 소스 계보이고, 커널, initramfs, squashfs 같은 큰 대상을 연다. 버전 가족은 1.7.3, 1.8.1, 1.8.2에서 바뀌었다는 NEWS의 근거가 있으니, 여러 버전으로 fixture를 만들어 겹치는 가족부터 잰다.
- **LZMA의 이득은 안쪽 내용에서 나온다.** liblzma 인코더는 2012년 이후 출력이 거의 안 바뀌었지만, 이미 강한 코더라 다시 부호화해서 얻을 것은 적다. aqr-lzma의 10gb 이득(−38.8 MB)도 모두 tar 멤버가 설치된 파일과 공유되어 나왔다. xz와 7z도 같은 이유로 안쪽이 다른 곳에 있을 때 값어치가 있다.
- **zstd는 같은 버전이면 빌드 사이에 결정적이다**(`ZSTD_isDeterministicBuild`). 1.5.x 최신 판의 레벨 1–4(fast, dfast)만 포팅하는 것이 aqr-lz4 방식의 연장이다. 입력이 이미 zstd인 경우가 늘고 있어 장기적으로 큰 영역이다.
- **LZX와 Oodle은 재현보다 aqr-lzma식 parse 레코드를 먼저 검토한다.** 비트스트림은 공개 디코더로 규명되어 있다. 다만 원본 바이트를 내려면 Huffman·tANS 표도 재현해야 하므로, 표 구성 규칙이 결정적인지가 관건이다(미검증). Oodle은 ooz의 라이선스 때문에 클린룸이 필요하다.
- **GDeflate는 값싼 보너스이다**(swizzle 역변환 뒤 aqr-flate). LZF는 Snappy 수준의 작은 작업이다(Krita, Redis).
- fixture로 확인할 것: lz4 1.9.x와 1.10.0 fast의 동일성, lz4-java(Minecraft)의 출력.


## 4. JPEG

aqr-jpeg의 모델과 수치는 그 README(Design, Tried and dropped)에 있다.

### packJPG [분석]

- https://github.com/packjpg/packJPG, LGPL-3.0. 2.5k(2016)에서 사실상 멈췄다(마지막 커밋 2020-04). "packJPG Multi-threaded" 포크는 찾지 못했다.
- 계수를 zigzag 위치별 평면으로 다시 배열해 이미지 전체를 위치 단위로 코딩한다(`colldata[cmp][bpos]`). 그래서 스트리밍이 안 되고 계수를 전부 메모리에 둔다.
- 블록별 nonzero 분포(7×7과 가장자리를 나눈다), DC, 7×7 AC, 가장자리 AC 순이다. 문맥은 이웃 6블록의 절댓값 가중 평균, 남은 nonzero 개수, 이웃 부호이다. 적응형 산술 코더이고, 비트 길이는 다중 심볼 모델로, 나머지 비트와 부호는 이진 모델로 코딩한다.
- 재현: 패딩 비트 값 하나, scan별 RST 오류 수, EOI 뒤 쓰레기를 저장하고, 그 밖의 편차는 거부한다. progressive를 지원하고 산술 코딩 JPEG는 지원하지 않는다. 단일 스레드이고, README의 비율은 약 20% 절감이다.

### Lepton, lepton_jpeg_rust [분석]

- C++: https://github.com/dropbox/lepton, Apache-2.0, 보관(archived) 상태. README가 Microsoft의 Rust 구현이나 JPEG XL로 옮기라고 권한다.
- Rust: https://github.com/microsoft/lepton_jpeg_rust, Apache-2.0, 활발(2026-08). aqr-jpeg의 출발점이었고 한 스레드 약 19 MB/s이다(aqr-jpeg README).
- 모델(`lib/src/structs/model.rs`, `lepton_encoder.rs`, `DESIGN.md`). 블록마다 7×7 내부 49개, 가장자리(행 0, 열 0) 14개, DC 순으로 코딩한다.
  - 7×7: 먼저 7×7 nonzero 개수를 위·왼쪽 블록 개수의 평균(9구간) 문맥으로 코딩한다. 계수 문맥은 위치, 남은 개수 구간, 위·왼쪽·왼쪽 위 블록 같은 위치의 절댓값 가중 평균이다.
  - **가장자리: nonzero 개수를 따로 두고, 이웃 블록과의 경계 픽셀 연속성을 계수 영역에서 푼 예측값을 문맥으로 쓴다**(`idct.rs`).
  - **DC는 마지막에 코딩한다.** 이때는 AC가 모두 알려져 있으므로 블록 가장자리 픽셀을 복원해 이웃과의 기울기에서 여러 예측을 만들고, 그 평균과 불일치를 쓴다.
  - Exp-Golomb식 이진화, 카운터 기반 적응 확률, VP8 bool coder를 쓴다.
- 재현: 헤더는 zlib으로 압축해 앞에 둔다. 패딩 비트, RST 개수와 오류, 쓰레기를 저장하고, canonical 재인코딩이 아니면 거부한다. 절단 파일을 일부 받는다.
- 병렬: 이미지를 가로 구간 최대 8개로 나눈다. 구간마다 Huffman 재개 상태(오프셋, 남는 바이트와 비트, 마지막 DC)를 헤더에 둔다. 모델은 구간마다 새로 시작한다. progressive는 Huffman 단계가 직렬이다.
- README의 비율은 평균 22% 절감이다.

### brunsli [분석]

- https://github.com/google/brunsli, MIT. 유지보수만 된다. 계수 모델이 JPEG XL의 JPEG 재압축(`jbrd`)의 바탕이다. `jbrd` 세부는 확인하지 못했다.
- 블록마다 빈 블록 플래그, nonzero 개수, 계수 순이다. 계수 문맥은 남은 개수와 주파수(`ZeroDensityContext`), 위·왼쪽 계수 가중 평균(8구간)이다. 가장자리는 블록 간 연속성 예측(`ACPredictContextRow/Col`)에서 부호와 평균 문맥을 얻는다. DC는 이웃에서 예측한다.
- ANS 코더를 쓰고, 파일별 히스토그램과 문맥 맵 클러스터링(`cluster.h`, `context_map_encode.cc`)을 전송한다.
- 재현: 0과 1이 섞인 패딩 비트 큐, RST 번호를 검증하고(틀리면 실패), 모르는 마커는 거부하며, 마커 사이 데이터는 따로 저장한다. 스레드는 쓰지 않고, README의 비율은 22% 감소이다.

### paq8px JPEG 모델 [분석]

- https://github.com/hxim/paq8px, GPL-2.0 이상, 활발. `src/model/JpegModel.cpp`(1,472줄).
- **Huffman 비트스트림을 직접 모델링한다.** 모델 안에서 Huffman을 풀면서 원래 비트를 예측하므로 canonical 가정이 필요 없고, 비정상 인코더 출력과 임의의 패딩을 그대로 받는다.
- 문맥은 30여 개 해시이다. 이웃 블록 계수, 위치, 경계 연속성 예측(`advPred`), run 예측, 직전 계수, 성분, Huffman 코드 자체가 들어간다. bit history StateMap, StationaryMap, 믹서, APM 14개를 거친다.
- progressive와 산술 코딩은 지원하지 않는다. SOF0/SOF1이 아니면 일반 모델로 넘어간다. 비트 단위 CM이라 매우 느리다.

### GDCC 2023 과제 1 [개요]

- https://gdcc.tech/results2023/ 의 정적 CSV에서 읽었다. Balanced 부문 하나이고, 데이터셋 993.5 MB, 제출 94건 중 31건 통과.

| 순위 | 이름 | 압축 s | 해제 s | 출력 | 비율 |
|---|---|---|---|---|---|
| 1 | GJm | 12.83 | 30.18 | 324,188,218 | 0.326 |
| 2 | djaz | 16.83 | 20.72 | 326,497,492 | 0.329 |
| 3 | baconjpgtxtrename | 15.61 | 20.42 | 522,916,778 | 0.526 |
| 4 | vchead | 4.94 | 33.25 | 713,465,525 | 0.718 |

- 0.33은 JPEG 재압축이 아니라 데이터셋의 중복에서 나온다. 대회 뒤 스레드 4048(#56–#58)에 공개된 구성으로는 979개 파일이 원본 75장의 다품질 사본이다. 모두 libjpeg(thorfdbg)로 인코딩했고 크로마 서브샘플링이 없다. 규칙상 입력 디렉터리 전체를 아카이브 하나로 만들고, 주최 측도 이미지 간 유사성을 쓰라고 권했다.

| 묶음 | 파일 | djaz의 비용 |
|---|---|---|
| HQ 컬러 | 74 | 290.25 MB |
| HQ 그레이스케일, 독립 1장(합성 "zone" 이미지) | 1 | 1.99 MB |
| HQ 그레이스케일, 컬러 HQ 14장의 Y 채널 | 14 | 20.14 MB (컬러 HQ로부터의 delta) |
| LQ, HQ 89장 × 품질 10단계 | 890 | 14.12 MB (HQ로부터의 delta) |

- 위 표는 cfeck이 공개한 수치이고, 합계 326.50 MB가 djaz의 출력과 같다. salutte가 공개한 324.75 MB(HQ 컬러와 그레이스케일 311.57, LQ delta 13.18)는 GJm에 가깝지만 일치하지는 않는다.
- LQ는 같은 픽셀을 다른 양자화표로 인코딩한 것이라 HQ 계수를 재양자화한 값과 반올림 오차만큼만 다르다. pgcm은 delta 대신 가능한 계수 조합 약 9만 개의 룩업표로 엔트로피 코딩 없이 약 17 MB를 얻었다. 그레이스케일은 libjpeg의 RGB→Y 변환을 내림(truncate)한 값이다(salutte). 이를 정확히 쓰는 판은 해제에 15분이 걸려 버려졌다. salutte는 "zone" 이미지를 역공학해 수 KB로 줄였다.
- 실제 재압축은 HQ 컬러 74장뿐이다. 같은 74장에서 brunsli는 약 279 MB, djaz는 290 MB이다. 원본 크기는 공개되지 않았으나 brunsli의 보통 비율로 보면 350–370 MB이고, 나머지 약 630 MB의 사본이 약 35 MB로 접힌다(추정).
- 3위 baconjpg는 HQ를 재압축하지 않고 인접 이미지 delta와 RLE만 썼다. 이미지 간 중복을 못 쓰면 paq8px solid도 0.675(670.9 MB), brunsli 단독은 0.765에 그친다. 이 중복은 계수를 재양자화해야 보이기 때문이다.
- 해제 시간 측정은 한동안 VM의 썸네일 생성과 wall time 측정 때문에 부풀었다가 2023-08에 고쳐졌다(#28–#41).
- 엔트리 대부분은 이 데이터셋에만 맞춘 것이고, 상위 두 개의 소스와 하드웨어, 스레드 수는 찾지 못했다. aqr-jpeg의 비율과 비교할 수 없다.

### packARC, 그 밖의 JPEG·MP3 도구 [개요]

- packARC(https://github.com/FS-make-simple/packARC, LGPL-3.0, 0.7RC18, 2014-12): packJPG, packMP3, packPNM, packARI를 묶은 `.pja` 아카이버이다(`source/packANY/`). 확장자와 시그니처로 코덱을 고르는 단순한 구조이다. packPNM의 소스는 이 패키지 안에만 있다.
- WinZip 12(2008)의 JPEG 방식: 비공개이다. 업체는 20–25%를 주장하고, PCWorld의 실측은 16–20%였다. Stirner의 코드를 쓴다는 설은 확인하지 못했다.
- StuffIt의 JPEG 재압축, UPGRADE_PAQJPVG, jojpeg, mp3dump, MP3 repacker: 공개 소스나 문서를 찾지 못했다.


### Aqueous에 대한 시사점

- Lepton·brunsli에서 온 후보(가장자리 계수의 경계 연속성 예측과 부호, 7×7과 가장자리의 nonzero 개수 분리, 픽셀 영역 DC)는 2026-10-25에 재 보고 모두 기각했다. 수치는 aqr-jpeg README의 "Tried and dropped"이다. 문맥 맵 클러스터링(brunsli)은 aqr-jpeg의 그룹별 정밀도와 부모 분포 상속과 같은 문제를 푼다.
- 비권장: Huffman 비트스트림 직접 모델링(paq8px)과 위치별 평면 코딩(packJPG)은 속도와 메모리 목표에 맞지 않는다.
- aqr-jpeg가 앞선 점: 속도(lepton_jpeg_rust의 약 2–4배), 투기적 병렬 Huffman 디코딩, stripe 단위 메모리, progressive의 인라인 canonical 판정, 퍼징(`mutenc`).


## 5. 오디오

### packMP3 [분석]

- https://github.com/packjpg/packMP3, LGPL-3.0(1.0d까지 GPL-3.0). 1.0g(2016-01)이 마지막이다. C++, 본체는 `source/packmp3.cpp`(8,144줄), `aricoder.cpp`, `huffmp3.cpp`.
- 자체 보고: 무작위 MP3 6,000개에서 평균 16% 감소(`Readme.txt`). 속도 수치는 없다.
- 엔트로피 코더: 31비트 정수 산술 코더와 PPM형 적응 모델 `model_s`(order ≤ 2, escape로 문맥을 잇는다). 정적 표는 없고 전부 파일 안에서 적응한다.
- 계수 모델(`pmp_encode_main_data`, 약 4540–4990행):
  - 고주파에서 저주파로 **역순**으로 count1 영역, region 2, 1, 0을 코딩한다. 같은 granule에서 이미 코딩한 상위 계수의 감쇠 평균 `ctx_abs = (2·abs[p] + ctx_abs + 2) / 3`이 첫 문맥이다.
  - 둘째 문맥 `ctx_h_abs[i]`는 같은 채널의 이력이다. granule마다 `(3a[i] + a[i−1] + a[i+1] + 2h[i] + 4) / 7`로 갱신하므로 직전 granule 하나가 아니라 지수 이동평균이다.
  - 심볼은 Huffman 쌍이 아니라 계수 하나의 크기(0..15)이다. 모델은 `mod_abv[ch][flags][table_select]`로 채널, 플래그(블록 종류, MS, 두 채널의 블록 종류 차이), Huffman 표마다 따로 둔다.
  - **부호도 모델링한다**(`mod_sgn`). 문맥은 `ctx_h_abs`와 지난 4 granule의 부호 이력(4비트)이다. 채널 1은 같은 granule 채널 0의 크기, 부호, 길이를 이력으로 받는다.
  - linbits: escape 길이를 `mod_len[ch][linbits]`에서 이력 문맥으로, 나머지 비트를 (길이, 비트 위치) 문맥으로 코딩한다.
  - 스케일팩터는 같은 granule의 앞 값과 앞 granule 같은 위치의 값으로, global_gain은 파일 전체 통계에서 만든 문맥(`pmp_build_context`)으로 코딩한다. `big_values`, region 경계, count1 시작(`sv_bound`)을 명시적으로 코딩한다(역순 코딩에 먼저 필요하다).
- 정확한 재현: 파일 전체를 프레임 격자로 보고 앞뒤(ID3v2, ID3v1)는 order-1로 코딩한다. 패딩, stuffing 비트, 보조 데이터(LAME ancillary는 예측), `main_data_begin`은 구조에서 계산하고 CRC는 유도한다.
- 거부 조건(1578–1693행, 1889–1930행): MPEG-1 Layer III가 아님, CRC 사용·intensity stereo·emphasis·private/copyright/original 비트가 프레임마다 다름, mixed block, 0이 아닌 패딩 비트, 동기 실패나 끝의 쓰레기. VBR 헤더 프레임과 free format은 확인하지 못했다(아마 미지원).
- 따라서 16%는 거부한 파일을 뺀 모집단의 값이다. aqr-mp3는 MPEG-2/2.5, free format, mixed block, 불일치 비트, 쓰레기와 절단을 받는다.

### balrogg [분석]

- https://github.com/iczelia/balrogg, GPL-3.0. 1.0(2026-09-03), 커밋 하나짜리 스냅샷이다. C99, 약 1만 줄(`vorbis.c` 1,463줄, `opusmode.c` 1,571줄, `cm.c`, `cmmix.c`, `residue.h`). 포맷은 2.0 전까지 호환을 약속하지 않는다.
- README 수치: `.ogg` 8–12%, `.opus` 3–8%, 최대 15%.
  - 파일 1,047개(79.3 MB): `-1` 90.4%, `-4` 88.6%, `-9` 88.2%. `-9`는 14초(병렬), 프로세스당 17 MB.
  - piano3.ogg(2.5 MB): `-1` 83.3%, `-2` 79.4%, `-4` 78.9%, `-9` 78.3%. 대부분은 `-2`까지에서 나오고, 그 위는 파라미터 탐색으로 0.1–0.6 pp이다.
  - 인코딩 때 설정 후보마다 파일 전체로 튜닝을 돌려 가장 좋은 것을 헤더에 적는다.
- Vorbis(`vorbis.c`, `residue.h`):
  - 셋업 헤더의 코드북을 파싱해 codeword 할당(`_make_words`)을 재현한다. 코드북의 codeword 길이와 floor·residue의 북 선택은 1차 체인으로 코딩한다.
  - Floor 1: post를 X 오름차순으로 코딩하고, 0 이력을 문맥에 넣는다(`vorbis.c:1003`).
  - Residue: lookup type 1 엔트리를 중심 정렬한 base-nv 자릿수로 풀어 **VQ 엔트리의 스칼라 값**을 심볼로 쓴다(`vorbis.c:1105`). 문맥은 같은 bin의 앞 자릿수, 인접 자릿수, 채널 간 이웃, 파티션 클래스이다(`residue.h:69`).
  - 엔진: 해시한 bit history, state map, SSE2/AVX2 8입력 로지스틱 믹서(lpaq 계열), 링 버퍼 2^18과 최소 길이 10의 match model. 믹서는 높은 단계에서만 쓰고 `-1`은 단순 적응 확률이다.
  - 비지원: floor type 0, lookup type 2. 패킷 패딩과 단축 패킷은 바이트 모델로, 대체 subclass 선택과 classword는 별도 정정으로 처리한다. 체인 Ogg는 링크 간에 모델을 공유하고, 깨진 CRC나 잘못된 페이지 순서는 거부한다.
- Opus(`opusmode.c`, `src/opus/`는 libopus 파생): range decoder를 "driven" 모드로 고쳐 SILK·CELT의 심볼 지점(`OREC_S_SILK_VAD`, `NLSF_CB1`, `PULSES`, `SHELL`, `CELT_*` 등)을 뽑고, 심볼 단위로 모델링한 뒤 디코딩 때 range encoder로 다시 합성한다. 확률 표가 고정이라 재인코딩이 결정적이다. mono/stereo family 0과 61,440 B 이하 패킷만 받는다. 이득이 Vorbis의 절반쯤인 것은 range coder의 표가 이미 합리적이기 때문으로 보인다(추정).

### oggre, ogg_rehuff, rehuff, OptiVorbis [개요]

- oggre: 공개 소스를 찾지 못했다. 스레드 3256에서 표본 하나를 26% 줄였다.
- ogg_rehuff(Shelwien): nishi.dreamhosters.com에 `ogg_rehuff.rar`(2012-04)만 있고 받지 않았다. 같은 표본에서 18%.
- rehuff(Segher Boessenkool, https://wiki.xiph.org/Rehuff): 코드북은 두고 Huffman 길이만 2패스로 다시 최적화한다. 1–3%, 2002년 이후 비활성, 라이선스 불명.
- OptiVorbis(https://github.com/OptiVorbis/OptiVorbis, 라이브러리 AGPL-3.0 또는 BSD-3-Clause, CLI는 AGPL-3.0, Rust, 활발): 코드북 길이 재할당, 쓰지 않는 끝 엔트리 절단, Ogg 페이지 최대 패킹, 패딩·빈 패킷·비 Vorbis 스트림 제거. 디코딩한 PCM만 같고 원본 바이트는 돌아오지 않는다.
- 둘 다 원본 바이트를 복원하지 않으므로 비교 대상이 아니다. 이득은 문맥 모델링에 흡수된다.

### Aqueous에 대한 시사점

packMP3와의 격차에서 남은 후보(코드 읽기에서 나온 가설이며 정량 검증은 하지 않았다). 시간 방향 이동평균(packMP3의 `ctx_h_abs`)은 2026-10-25에 넣었다(aqr-mp3 README).

1. **같은 granule의 이미 코딩한 이웃.** aqr-mp3는 직렬 의존을 피하려고 일부러 쓰지 않는다. README의 측정으로 직전 쌍이 0.53 pp(디코딩 약 −40%), 두 쌍 전이 0.37 pp(−14%)이다. 격차의 가장 큰 몫으로 보인다.
2. **부호 모델링.** aqr-mp3는 부호를 그대로 복사한다. 이득은 작을 것이다(1 pp 미만 추정). 비트당 이진 코딩이라 속도 비용이 크다.
3. **심볼 단위.** packMP3는 계수 단위에 인접 계수를 조건으로 두고, aqr-mp3는 쌍 단위 심볼에 문맥을 500표로 병합한다.
4. **적응 대 정적.** 인코더와 비트레이트가 섞인 코퍼스에서는 적응이 유리할 수 있다. 정적 prior 위에 SSE/APM이나 2입력 믹싱을 얹는 혼합을 시도할 만하다.
5. 부가정보(1.7%)와 linbits 하위 비트는 격차를 설명하지 못한다. packMP3가 `sv_bound`와 region 경계를 명시 저장하는 것은 오히려 손해일 수 있다.

시도 순서: (a) 채널 1에 채널 0의 인접 쌍 합, (b) 직렬 의존을 허용하는 별도 레벨(`max`)에서 역순 코딩과 부호 모델, (c) 정적 확률 뒤 SSE. 격차가 어디서 오는지는 packMP3를 `tools/compare.sh`의 FMA 402개에 돌려 계수, 부호, 스케일팩터, 부가정보로 나누어 재면 확정할 수 있다.

Vorbis:

- aqr-vorbis(Vorbis I 명세만으로 작성)는 FSB5에 담긴 패킷을 받는다. The Crew의 FSB5에서 레코드가 큰 스트림의 90.7%, 작은 스트림의 93.2%이고, 한 스레드로 약 23 MB/s, 21 MB/s이다. balrogg `-1`(90.4%)과 비슷하고 `-9`(88.2%)보다 2 pp 남짓 크다. 표본이 달라 그대로 견줄 수는 없다.
- `.ogg`에는 Ogg 페이지와 패킷의 재구성(CRC, 페이지 경계, 체인 스트림)이 남았다.
- balrogg에 있고 aqr-vorbis에 없는 것은 residue의 채널 간 이웃 문맥, 비트 단위 혼합, match model이다. aqr-vorbis는 속도 때문에 CM 대신 정적 표 rANS를 쓴다. balrogg는 GPL-3.0이므로 설계만 참고한다.
- Opus는 가능하다는 것이 입증되었지만 이득이 3–8%라 후순위이다.

### 무손실 오디오: 기준

aqr-flac은 FLAC 잔차를 32탭 sign-sign LMS와 정적 표 rANS로 코딩한다. IA 세트 0.970, synth 0.821, 압축 63–77 MB/s, 복원 159–198 MB/s이다.

aqr-flac README의 "Tried and dropped"에 이미 측정된 것들:

| 시도 | 결과 |
|---|---|
| LMS 캐스케이드(32 + 16, 256 + 16), float NLMS | 32탭 하나보다 0.06 pp 이내 |
| 채널 간 탭 | 0.1 pp 미만 |
| 샘플 도메인 재예측 | −3.0%(잔차 위 LMS는 −3.6%) |

비교로 FFmpeg의 최고 WavPack은 `flac -8`보다 1–2% 작았고, TTA는 거의 같았다.

### Sac [분석]

- https://github.com/slmdev/sac, MIT, 0.7.18(2025-04).
- 입력은 1–16비트 mono/stereo WAV만 받는다. 내부가 fp64라 플랫폼에 따라 출력이 달라질 수 있고, 저자가 "시험용"이라 밝힌다.
- 예측(`libsac/pred.cpp`)은 채널마다 세 단계이다.
  1. **OLS**(`pred/ols.cpp`): 지수 감쇠(λ ≈ 0.998) 가중 공분산을 Cholesky로 주기적으로 다시 푼다. IRLS 가중으로 L1과 L2 사이를 조절한다. 채널 1은 채널 0의 과거 8개와 현재 이후 8개 샘플을 입력으로 받는다.
  2. **NLMS 캐스케이드**(`pred/cascade.h`, `nlms.h`): 기본 1280/256/32탭에 RLS 한 단을 더한다. 단계 출력은 `BlendLS`로 섞는다. 전문가는 L1·L2 손실의 둘이고, 가중치는 최근 |오차| 지수 이동평균의 softmax이다.
  3. **BiasEstimator**(`pred/bias.h`): 앞 입력과 델타의 부호·크기 비트로 만든 문맥(64개씩 셋)마다 평균 편향을 세고, sign-sign LMS로 섞어 예측에 더한다.
- 코더는 비트 평면, 카운터, 믹서, SSE, range coder이다. 인코더가 프레임마다 메타파라미터를 탐색해 매우 비대칭이다.
- README 표(44.1 kHz 스테레오 16개, 평균 bps):

| 코덱 | bps | FLAC -8pe 대비 |
|---|---|---|
| Sac --best | 8.303 | 89.0% |
| OptimFROG 5.100 max | 8.493 | 91.0% |
| paq8px -6 | 8.530 | 91.4% |
| MPEG-4 ALS | 8.718 | 93.4% |
| Monkey's Audio | 8.817 | 94.5% |
| FLAC -8pe | 9.330 | 100% |

  - Sac `--normal`이 이미 90.1%이다. `--best`(89.1%)의 탐색은 마지막 1 pp를 사는 데 쓰이고, 인코딩 4시간 37분, 디코딩 1분이 걸린다. OptimFROG max는 9초와 2초이다.

### Monkey's Audio [분석]

- https://www.monkeysaudio.com(13.43). 구조는 비공식 유닉스 포트 https://github.com/ralph-irving/mac 으로 읽었다. 라이선스 문구는 확인하지 못했다.
- 예측(`src/MACLib/NewPredictor.cpp`, `NNFilter.cpp`): 1차 고정 필터와 적응 오프셋 필터 뒤에 정수 sign-sign LMS("NN filter")를 직렬로 쌓는다.
  - 레벨별 (탭, 시프트): normal (16, 11), high (64, 11), extra high (256, 13)+(32, 10), insane (1024+256, 15)+(256, 13)+(16, 11).
  - 가중치는 int16이고 입력은 부호와 크기를 몇 단계로 양자화한다. **샘플 도메인**에서 돈다.
- ERK 2023(Železnik, 16비트 스테레오 61개, https://erk.fe.uni-lj.si/2023/papers/zeleznik(overview_of).pdf):

| 코덱 | 압축비 | 압축 s | 해제 s |
|---|---|---|---|
| FLAC 1.3.4 --best | 1.794 | 7.94 | 7.99 |
| MPEG-4 ALS RM23 -b | 1.830 | 16.44 | 13.01 |
| Monkey's 10.16 -c4000 | 1.865 | 14.30 | 14.20 |

  - 크기로는 FLAC보다 ALS가 2.0%, Monkey's가 3.8% 작다.

### WavPack, ALAC [분석]

- WavPack(https://github.com/dbry/WavPack, BSD-3-Clause, 5.9.0, 2026-01):
  - 샘플 도메인의 decorrelation 단계 연쇄(항 −3–18, 부호 기반 적응)이다. `-x1`–`-x6`는 인코더가 항 조합을 탐색할 뿐 디코딩 비용은 같다. 엔트로피는 적응 median 셋의 Golomb형으로 알고 있다(미검증).
  - 같은 저자의 Newpack(https://github.com/dbry/newpack)은 오디오가 아닌 범용 압축기이다.
- ALAC(https://github.com/macosforge/alac, Apache-2.0, `codec/`):
  - 4096프레임 패킷, 정수 매트릭스 믹스(`mixBits`, `mixRes`) 한 번, 채널마다 헤더에 계수를 둔 차수 최대 31의 LPC(`dp_enc.c`)이다. 디코딩 중 계수를 부호 기반으로 샘플마다 적응한다.
  - 잔차는 적응 Golomb(`ag_enc.c`)이다. 평균 추정을 `mb = pb·(n + zmode) + mb − ((pb·mb) >> QBSHIFT)`로 갱신하고, 평균이 작으면 0 런 모드로 간다. 상수는 PB0=40, MB0=10, KB0=14, QBSHIFT=9이다. 접두사를 넘으면 원시 비트로 이스케이프한다.
  - 24비트는 하위 바이트를 따로 둔다(`bytesShifted`). 압축이 더 크면 PCM 그대로 둔다.
  - **Rice 파라미터가 잔차 열의 순수 함수**라, 잔차만 알면 비트스트림이 정확히 재현된다. 합성이 필요 없다.

### TTA, MPEG-4 ALS, Shorten, OptimFROG, TAK, FLACOut [개요]

- TTA(LGPL-3.0, 예 https://github.com/gchudov/libtta): 고정 1차 필터, sign-LMS 적응 필터, 적응 Rice이다. 결정적으로 보이지만 확인하지 못했다.
- MPEG-4 ALS(RM23): 블록 스위칭, 차수 1023까지의 LPC, 장기 예측, 채널 간 예측, Rice/BGMC, 선택적 RLS-LMS 모드이다. 파일이 드물다.
- Shorten: 고정 다항식(차수 0–3)이나 블록 LPC에 Rice이다. etree의 옛 라이브 녹음에 있다.
- OptimFROG, TAK: 닫힌 소스이다. TAK는 Monkey's high와 extra high 사이의 압축률에 디코딩 CPU는 그 1/7–1/10이라고 한다(HA 위키의 2차 인용).
- FLACOut: 닫힌 바이너리(Ken Silverman)이다. 공개 재구현 flacoutcpp(https://github.com/Snesnopic/flacoutcpp, MIT)는 libFLAC 위에서 가변 블록 크기를 DP로 탐색하고, 창 함수와 차수 후보를 매긴다. 그래서 출력은 **가변 블록 크기 스트림**이다.
- hac(스레드 4180): 공개 코덱을 찾지 못했다.

### Aqueous에 대한 시사점(무손실 오디오)

- **PCM 경로(aqr-raw)에서 샘플 도메인 캐스케이드를 시험한다.** FLAC 경로에서는 FLAC의 LPC 위라 캐스케이드가 듣지 않았다. PCM은 aqr-raw가 직접 LPC를 고르므로 Monkey's식 int16 sign-sign LMS를 직렬로 쌓을 수 있다. 예를 들어 high는 64탭, extra high는 256 + 32탭이다. Monkey's는 FLAC보다 3.8% 작다(ERK). 샘플당 약 20–40 사이클이 더 들 것이다(미측정).
- **Sac식 편향 보정**(`pred/bias.h`): 부호·크기 비트 문맥 셋의 카운터 평균을 예측에 더한다. 샘플당 10사이클 안팎이고, Rice 파라미터 문맥(효과 없었음)과는 다른 정보이다. 분리된 이득 수치는 찾지 못했다.
- **두 전문가 혼합**: 스텝이 다른 sign-sign LMS 둘을 최근 |오차|로 섞는다. 정수 가중치 두 개로 된다.
- PCM이 L/R로 들어올 때 둘째 채널에 첫 채널의 같은 시점 샘플을 탭으로 넣는다(Sac). FLAC 경로의 0.1 pp는 FLAC이 이미 M/S를 해서이다.
- 엔트로피 코더는 그대로 둔다. Sac의 비트 평면과 SSE는 속도 목표에 맞지 않는다.
- **포맷 우선순위**:
  1. ALAC: 잔차 코딩이 결정적이라 aqr-flac과 같은 설계가 된다. MP4 컨테이너(`stsz`, `stco`) 분해가 필요하다.
  2. Shorten
  3. TTA(인코더의 결정성을 먼저 확인)
- **가변 블록 크기 FLAC 확인.** aqr-flac README는 flake와 일부 FFmpeg 설정이 만드는 가변 블록 스트림을 헤더 예측이 처리한다고 하지만, 그런 표본으로 시험했다는 기록은 없다. flacoutcpp와 flake로 표본을 만들어 보면 싸게 확인된다.
- Monkey's, WavPack, OptimFROG는 README의 판단(남는 것이 적음)이 맞다. 합성까지 하려면 인코더를 비트 단위로 재현해야 한다.



## 6. 이미지

### Precomp와 paq8px의 GIF 재현 [분석]

- Precomp(`precomp.cpp`의 `try_decompression_gif` 6407행, `recompress_gif` 6129행, `contrib/giflib/egif_lib_gcc.c`의 `EGifCompressLine`):
  - 디코더가 LZW 코드열과 픽셀을 함께 뽑는다. giflib 인코더(탐욕적 최장 일치, `RunningCode >= MaxCode1`이면 코드 폭 +1, 표가 차면 clear)를 다시 돌려 다른 곳만 `GifDiffStruct`에 적는다.
  - 차이는 네 종류이다. 원본만 중간에 clear를 넣은 곳, 표가 찼는데 원본이 clear 없이 계속한 곳(deferred clear), 최장 일치가 아닌 코드, 그 밖이다.
  - 인코더 모델이 하나뿐이라 Photoshop, ImageMagick, gifsicle 출력에서는 차이가 많아질 것이다(추정). 픽셀은 이미지 모델 없이 LZMA2로 보낸다.
- paq8px(`src/filter/gif.hpp`, GPL-2.0):
  - clear 위치를 사전 크기 하나로 가정하고, 최장 일치가 아닌 위치를 4바이트 목록(최대 4,096개)으로 둔다. 다시 인코딩해 다르면 포기한다.
  - 푼 픽셀은 8bpp 이미지 모델로 보낸다. 이 모델은 이전 프레임에서도 예측하고, 팔레트로 회색조를 감지한다(CHANGELOG).

### flexiGIF [개요]

- https://create.stephan-brumme.com/flexigif-lossless-gif-lzw-optimization/ (2018.11a). 저장소는 없고 라이선스는 확인하지 못했다.
- 픽셀은 두고 LZW만 다시 쓰는 최적화기이다. 1단계 lookahead 파싱, deferred clear, 무차별 탐색으로 찾은 리셋 지점, 첫 clear 생략(`-y`), 사전을 4093으로 제한(`-c`)을 쓴다.
- 대부분 약 2% 줄인다. 예시 하나는 363,021 B에서 339,011 B였다(gifsicle은 347,919 B).
- 원본 바이트를 보존하지 않으므로 재압축기는 아니다. 다만 인코더가 낼 수 있는 선택지를 한눈에 보여 준다.

### packPNM, CryoPNG [개요]

- packPNM(https://github.com/packjpg/packPNM, LGPL-3.0, 커밋 5개): PPM, PGM, PBM, BMP를 받고, 1.6에서 Radiance HDR을 실험적으로 지원한다. 알고리즘 문서가 없고 소스는 읽지 않았다.
- CryoPNG: 공개 자료를 찾지 못했다.

### 카메라 RAW [개요]

- rawspeed(https://github.com/darktable-org/rawspeed): `src/librawspeed/decompressors/`에 디코더 29개가 있다.
  - lossless JPEG: `LJpegDecompressor`, `Cr2Decompressor`, Hasselblad.
  - 제조사별: Nikon, Olympus, Fuji, Kodak, CRW, Pentax, PhaseOne, Panasonic V4–V8, Samsung V0–V2, Sony ARW1/ARW2.
  - 기타: GoPro VC5, DNG의 deflate, JPEG, 비압축.
  - 디코드 전용이다.
- 바이트 정확 RAW 재압축기는 공개 오픈소스에서 찾지 못했다.
  - Rawzor는 상용이고 20–60%를 주장하지만, 근거는 포럼 일화 하나(RAF 8.4 MB에서 7.8 MB)이다.
  - PackRAW와 rawdet는 공개 자료를 찾지 못했다.
  - dnglab은 RAW를 압축 DNG로 바꾸는 손실 없는 변환(원본 바이트는 아님)이다. CR2에서 11–23%, NEF(D800)에서 44% 줄었다고 한다.

### 텍스처 [분석, 일부 개요]

- crunch(https://github.com/BinomialLLC/crunch, public domain, 2025-05):
  - .crn은 **손실**이다. 블록을 endpoint 코드북과 selector 코드북으로 군집화하고(`crn_dxt_hc.cpp`, `crn_clusterizer.h`), 인덱스를 델타로 쓰고, 정적 Huffman으로 부호화한다.
  - RDO 모드는 DXT 포맷을 두고 LZ에 유리하게 블록을 바꾼다.
  - 무손실에 쓸 만한 것은 endpoint와 selector의 분리, 이웃 델타라는 구조뿐이다.
- BC7Prep(Oodle Texture, 상용, 특허 출원): 비트 재배치 변환이다. BC7 + Kraken이 1.387:1에서 1.530:1이 된다(약 10%). 역변환은 GPU 셰이더로 할 만큼 빠르다.
- dxt-lossless-transform(https://github.com/Sewer56/dxt-lossless-transform, GPL-3.0): BC1–BC3는 거의 완성, BC7은 시작 단계이다. zstd 기반 추정기로 변환 설정을 고른다.
- Basis Universal(https://github.com/BinomialLLC/basis_universal, Apache-2.0): KTX2와 zstd 초압축을 쓴다. XUASTC의 산술 부호화 프로파일은 zstd보다 5–18% 낫다고 한다. 표준 ASTC를 바이트 정확히 재현하는 도구는 아니다.
- BCPack(Xbox): 공개 자료를 찾지 못했다.
- paq8px에는 DDS/BCn 모델이 없다.
- 범용 LZ 비교(cbloom, 게임 BCn 세트): zlib 2.72, zstd 3.17, lzma 3.69, Kraken 3.67, Leviathan 3.84.
- PSD와 TIFF의 공개 최적화기·재압축기는 찾지 못했다.

### Aqueous에 대한 시사점

- **GIF.** "탐욕 LZW 재인코딩과 차이 목록"(Precomp, paq8px)이 가장 쉬운 시작점이지만, 인코더 모델이 하나라 한계가 있다. aqr-flate처럼 인코더의 선택을 프로파일로 두고, 예측과 다른 코드만 정정으로 남기는 편이 낫다.
  - 선택지: clear 시점(가득 찼을 때, 지연, 중간), 코드 폭이 늘어나는 시점(구현마다 하나씩 어긋난다고 알려져 있다, 미검증), 탐욕 대 lookahead, 첫 clear 생략, 사전 4093 제한.
  - 코드열을 새 항목·기존 항목·최장 일치 여부의 기호로 두고 픽셀 문맥으로 모델링하면 aqr-png의 LZ77 토큰 모델링과 같은 구조가 된다.
  - 애니메이션에는 이전 프레임 문맥(paq8px)을 쓰고, 팔레트 인덱스에는 인덱스 도메인 문맥(이웃과 같은지, 팔레트 정렬)을 쓴다.
- **camera RAW.** 공개 선례가 없으므로 aqr-jpeg의 Huffman 디코더를 SOF3로 넓히는 것이 주 작업이다. 재현할 것은 Huffman 표, 예측기 1–7, scan과 restart 구조이다. 모델은 Bayer 평면별 문맥, 2차원 잔차, black level과 비트 깊이의 상한을 쓴다.
  - 구현 순서는 rawspeed의 디코더 목록을 지도로 삼는다. CR2와 DNG(LJpeg, deflate), NEF, ARW2, 그다음 RW2, ORF, PEF, IIQ, RAF이다.
  - ARW2는 엔트로피 부호화가 없어 이득이 가장 크다(README와 같은 판단). 컨테이너는 TIFF/EXIF이므로 aqr-raw의 TIFF 파서(지금은 비압축만)를 넓혀 함께 쓴다.
- **텍스처.** BC1–BC5는 aqr-tex가 한다. 남은 것은 BC6H, BC7, KTX이다. BC7은 모드별 가변 비트 필드, 파티션, p-bit를 나눠야 하고, 예약 모드 같은 무효 인코딩도 고려해야 한다. BC7Prep의 10%가 범용 LZ 위의 기준선이고, BC7Prep에 특허 출원이 있으니 청구항을 확인해야 한다.
- Precomp의 GIF와 PNG는 이미지 모델 없이 LZMA로 보내고, paq8px는 이미지 모델이 있지만 7 KB/s이다(aqr-png README). 빠르면서 이미지 모델을 가진 자리는 aqr-png와 aqr-raw 말고는 비어 있다.


### 무손실 이미지 코덱 [분석, 일부 개요]

aqr-raw 래스터 모델의 참고로 본다.

- fNBLI와 NBLI(https://github.com/WangXuan95/NBLI, GPL-3.0. 소스는 NBLI만 있고 fNBLI는 실행 파일만 배포하는 것으로 보인다):
  - CALIC·EDP·MRP 계열 예측과 문맥에, 엔트로피는 ANS(`-g`는 Golomb 산술 트리)이다. fNBLI의 새로운 점은 병렬 엔트로피 인코딩이라고 한다.
  - README의 CLIC2021 professional 585장, 한 스레드 수치:

| 코덱 | PNG 대비 | 압축 | 해제 |
|---|---|---|---|
| fNBLI | −29.8% | 94.8 MB/s | 149 MB/s |
| NBLI `-g` | −30.4% | 14.7 MB/s | 17 MB/s |
| JPEG XL e8 | −30.25% | 0.6 MB/s | 14.5 MB/s |

- QLIC(https://github.com/OzzyMitchell/QLIC, Apache-2.0, `docs/architecture.md`, `predictor-math.md`, `codec/src/qlic.c`):
  - 인코더가 모드 후보를 실제로 인코딩해 보고 가장 작은 것을 고르며, 디코더는 기록된 모드대로 한 번만 분기한다.
  - 가역 색 변환(G 델타, B 델타, `B − floor((wr·R + (d − wr)·G) / d)` 꼴의 가중 블렌드 등) 수십 종을 쓴다. 예측기는 바이트 필터, 클램프 그래디언트, MED이고, 문맥은 주변 절댓값 합의 activity와 오차 구간이다.
  - **디코딩을 30% 늦추면서 1%를 얻는 모드는 선택에서 뺐다**고 문서에 적혀 있다. 속도 예산을 다루는 방식으로 참고할 만하다.
  - 자체 주장은 3,167장에서 JPEG XL e9보다 0.30%, WebP 6보다 약 10% 작다는 것이다. 독립 검증은 없다.
- libjxl modular(BSD-3, `lib/jxl/modular/`):
  - MA 트리를 표본 일부에서 학습한다. 예측기는 Gradient와 self-correcting(Weighted, 6번)이고, cjxl 기본은 둘을 섞은 14번이다.
  - Weighted 예측기의 수식은 읽지 못했다. 하위 예측기 넷을 오차의 역수로 섞는 것으로 기억한다.
- QOI(https://github.com/phoboslab/qoi, MIT): 해시 인덱스, run, 작은 차이만 쓰고 예측과 문맥이 없어 참고로는 약하다.
- HALIC, GraLIC, BMF, LEA는 닫힌 소스이다. FLIF(MANIAC 트리, JPEG XL modular의 전신)는 보지 않았다.

### Aqueous에 대한 시사점(무손실 이미지)

- fNBLI의 압축 94.8 MB/s와 PNG 대비 −29.8%는 약 100 MB/s 목표가 비현실적이지 않다는 근거이다. NBLI는 GPL-3.0이라 아이디어만 쓴다.
- aqr-raw의 MED, 채널 상관 제거, activity 구간은 QLIC과 같은 계열이다. 값싸게 더 얻을 후보는 다음과 같다.
  - 이미지마다 색 변환을 고른다. 인코더 시간만 들고 디코더는 한 번만 분기한다.
  - 클램프 그래디언트와 MED 중 하나를 인코더가 고른다.
  - JPEG XL의 Weighted 예측기: 100 MB/s에서는 SIMD 없이는 비싸다.
- 인코더가 시험 인코딩으로 고르는 방식을 쓰려면, 디코딩 속도를 지키기 위해 느린 모드를 후보에서 빼는 규칙도 같이 가져와야 한다(QLIC).
- packPNM의 모델(packARC 안 `source/packANY/packPNM`)은 아직 읽지 않았다.


## 7. 문서·비디오·디스크 이미지

### Precomp의 PDF 처리 [분석]

- PDF 파서가 없다. 바이트를 훑다가 `/FlateDecode`(`precomp.cpp:3854`) 뒤 첫 `stream`에서 raw deflate를 시도하고(`try_decompression_pdf()`, 3325행), 복원 때 그 앞 헤더를 그대로 다시 쓴다.
- `pdfbmp` 옵션(3904–3983행, 기본 꺼짐): 앞 사전에서 `/Width`, `/Height`, `/BitsPerComponent`를 `strstr`로 찾아 8·24비트면 BMP 헤더를 씌워 이미지 모델에 넣는다. PNG 예측자와 `/DecodeParms`는 해석하지 않는다.
- JPEG은 PDF와 상관없이 전체 시그니처 스캔으로 packJPG나 brunsli에 넘긴다. 그래서 DCT 필터 체인은 문제가 되지 않는다.
- 암호화, ASCII85, 오브젝트 스트림, xref 처리는 없다.
- 바이트 정확 PDF 복원을 하는 다른 공개 도구는 찾지 못했다. qpdf(Apache-2.0)의 복호화, 오브젝트 스트림, xref 복구(`QPDF::reconstruct_xref`)는 파서 참고용일 뿐이다(복구는 방향이 반대이다). smpdf는 손실 도구로 보인다(조사하지 않음).

### ECM [분석]

- Neill Corlett, 2002, GPL(버전 불명). 공식 저장소는 없고 aminet, cmdpack, Debian 패키지로 전해진다. 현대 포크는 https://github.com/kidoz/ecm(GPL-2.0, `src/eccedc.c`, 포맷 문서 `doc/reference/format.md`), https://github.com/RDCH106/ECM, Rust 크레이트 `ecmlib`이다. DuckStation도 ECM 읽기를 넣었다.
- 포맷: `"ECM\0"`과 레코드(종류 2비트와 계속 비트의 가변 길이 개수). 종류 0은 리터럴이다.
  - 1(Mode 1)은 데이터 2048 B만 저장한다.
  - 2(Mode 2 Form 1)는 서브헤더 8 B와 데이터 2048 B를 저장한다.
  - 3(Mode 2 Form 2)은 서브헤더와 데이터 2324 B를 저장하고 EDC만 뺀다.
- 재구성: sync 12 B는 고정이고, 주소는 섹터 번호(MSF BCD)이다. EDC는 반사형 CRC-32 변형(다항식 0xD8018001)이고, ECC는 GF(2^8) 위의 RSPC P 패리티(43열 × 26행)와 Q 패리티(26대각 × 43)이다. 인코더는 섹터를 다시 만들어 원본과 같을 때만 그 종류로 정하고, 복제 방지용 오류 섹터 같은 불일치는 리터럴로 둔다.
- 이득은 Mode 1 데이터 섹터당 304/2352 B, 약 13%이다. 오디오 섹터와 cooked ISO에는 이득이 없다.
- GPL이므로 코드는 가져오지 말고 ECMA-130에서 새로 작성한다.

### YBE, CHD, ISO 9660/UDF, WIM [개요]

- YBE(스레드 4061): 공개 저장소를 찾지 못했다.
- CHD(MAME, `src/lib/util/chd.h`, `chdcodec.cpp`, `chdcd.cpp`, BSD-3-Clause): hunk 단위 컨테이너이고, v5는 hunk마다 압축기 네 개 중 하나를 고른다. CD 코덱은 cdlz, cdzl, cdzs, cdfl이다. 프레임마다 재생성할 수 있으면 sync와 ECC를 빼는 비트맵을 두고, 사용자 데이터와 서브코드를 따로 압축하는 것으로 안다(코드를 직접 읽지 못했다).
- ISO 9660: 디렉터리 레코드와 path table은 양쪽 엔디언 중복 필드이고, 볼륨 기술자와 path table은 디렉터리 트리에서 유도된다. 다만 크기는 파일 내용이 지배하므로, 실익은 파일 단위 재귀와 간격·패딩 복원에 있다. UDF는 태그 체크섬과 CRC-16, AVDP 세 벌 같은 유도 가능한 필드가 있다.
- WIM/ESD: 파일은 이미 SHA-1로 중복 제거되어 있다. 이득은 LZX/XPRESS/LZMS 인코더를 재현할 수 있는지에 달려 있다.

### 비디오 [개요]

- 공개된 무손실 H.264(CAVLC, CABAC) 재압축기는 찾지 못했다. H.264 lossless 모드 자체의 CABAC 개선 연구(최대 12.8%)는 인코더 쪽 이야기이다.
- MJPEG의 중복 제거와 Bink(RAD의 비공개 포맷)도 공개 자료를 찾지 못했다.
- 따라서 비디오는 선례 없이 aqr-jpeg와 aqr-mp3의 구조를 넓히는 자체 설계가 된다. MJPEG는 aqr-jpeg를 다시 쓰면 되므로 가장 싸다.


### Aqueous에 대한 시사점

- **aqr-pdf는 Precomp보다 앞선다.** 배울 것이 없다. aqr-pdf의 열린 항목(README의 Next steps) 가운데 다른 도구와 견줄 것은 이렇다.
  - 빈 사용자 암호의 암호화 파일: 공개 도구 중 하는 것이 없다. RC4와 AES-CBC(저장된 IV)의 재암호화는 결정적이라 구현이 단순하다. CC의 몇 %에 해당하므로 우선순위가 높다.
  - `[/FlateDecode /DCTDecode]`와 `[/ASCII85Decode /FlateDecode]` 체인: Precomp처럼 필터를 벗긴 뒤 안쪽 JPEG 시작을 찾으면 된다. ASCII85는 줄바꿈 규칙만 기록하면 가역이다.
  - xref 재구성: 공개 선례가 없고 이득은 18 MB 중 0.8 MB이다.
- **CD 이미지**(README "Worth doing"의 BIN/CUE 행)는 구현 비용이 낮다. EDC/ECC는 결정적이고 수백 줄이며, 불일치 섹터는 리터럴로 폴백하면 된다. 수요도 크다(Redump 덤프).
  - 섹터 모드 분류와 MSF 주소 예측을 하고, 예외는 리터럴 런으로 둔다.
  - Mode 2 서브헤더 8 B는 4 B가 중복이다.
  - 2336 B 섹터와 서브코드(96 B/프레임, Q 채널 CRC-16)도 받는다.
  - CD-DA 트랙은 aqr-raw의 PCM 경로로, 데이터 트랙의 ISO 9660은 파일 단위 재귀로 넘긴다.
- CHD, CSO, ZSO는 컨테이너로 풀어 같은 경로로 보낸다(README의 해당 행과 같다). WIM/ESD는 LZX 재현이 전제라 후순위이다.


## 8. 실행 파일

기준: aqr-exe(README)는 PE와 Mach-O의 x86, x86-64, ARM64, CIL을 받는다. x86 disfilter는 명령 필드별 스트림으로 나누고 상대 타깃을 절대로 바꾸며, 함수 사이 패딩을 부호화한다. ARM64는 `BL`과 `ADRP` 타깃을 절대로 바꾸고 지역 분기 오프셋을 스트림으로 뺀다. PE의 재배치는 전용 모듈이, export·import·runtime function 표는 aqr-infer-recs의 `recs-fields`가 맡는다. ELF와 RISC-V는 지원하지 않는다.

### xz BCJ 필터 [분석]

- https://github.com/tukaani-project/xz, `src/liblzma/simple/*.c`, 0BSD. 모두 제자리 변환이고 스트림 분리는 없다.
- `x86.c`: `E8`/`E9` 뒤 4바이트의 최상위 바이트가 00이나 FF인 것만 절대로 바꾼다. 5바이트 창의 `prev_mask`로 겹침과 오탐을 줄인다.
- `arm64.c`(5.4 이후):
  - `BL`의 26비트와 `ADRP`의 21비트 페이지를 절대로 바꾼다. `ADRP`는 ±512 MiB 안일 때만 바꾼다.
  - 짝이 되는 `ADD`/`LDR`의 lo12는 건드리지 않는다. 정렬이 어긋나면 4 KiB 단위로 틀리며, PE 안의 ARM64는 `start_offset=3072`가 좋다고 주석에 적혀 있다.
- `riscv.c`(5.6 이후, 773줄, 주석이 설계 문서 역할을 한다):
  - `JAL`은 rd가 x1/x5일 때만 바꾼다. `JAL x0`, `C.J`, `C.JAL`, 조건 분기, `C.BEQZ`/`C.BNEZ`는 절대 주소가 거의 한 번만 나와 손해이므로 두지 않는다.
  - `AUIPC rd≠x0`와 바로 다음 32비트 명령(`rs1 == AUIPC.rd`)의 **쌍을 통째로** 절대로 바꾼다. 다음 명령의 종류는 보지 않고 늘 I-type으로 다룬다. 그래서 store(S-type)는 부정확하지만 손익은 미미하다고 한다.
  - `AUIPC` 단독은 lo12를 몰라 4 KiB 단위로 어긋나므로 두지 않는다. `AUIPC x0`(LPAD)도 뺀다.
  - 2024년 기준 `AUIPC`의 99%가 연속 쌍이라고 한다. clang 16이 쌍을 떼어 놓는 코드(루프 밖 `AUIPC`, 사이의 분기)는 놓친다.
- RISC-V와 ARM64 필터의 공식 수치는 찾지 못했다. aqr-exe README의 비교(xz ARM64 BCJ는 zstd -9보다 12.1%, aqr-exe는 17.6% 작다)가 유일하다.

### 7-Zip BCJ, BCJ2 [분석]

- https://github.com/ip7z/7zip. `C/Bra.c`, `C/Bra86.c`에 x86, ARM, ARMT, PPC, SPARC, IA64, ARM64, RISCV(24.03)가 있다. xz의 RISC-V 구현은 독립 구현이지만 Igor Pavlov가 설계에 기여했다.
- BCJ2(`C/Bcj2.c`, `Bcj2Enc.c`, public domain)는 스트림 넷으로 나눈다.
  - `MAIN`: 나머지 바이트와 `E8`/`E9`/`0F 8x` 표식
  - `CALL`, `JUMP`: 절대 주소, 32비트 BE
  - `RC`: 표식마다 "바꿨는지" 1비트를 적응 이진 range coder로 부호화한 스트림. 문맥은 `E8`이면 앞 바이트(256), `E9`와 Jcc는 한 슬롯씩이다.
- 인코더는 상대 변위가 `relatLimit`(기본 `0x0F << 24`) 안이고 파일 범위 안일 때만 바꾼다. 디스어셈블은 하지 않는다.

### kkrunchy의 disfilter [분석]

- Fabian Giesen, https://github.com/farbrausch/fr_public 의 `kkrunchy_k7/dis.cpp`. public domain, 2003–2007. aqr-exe disfilter의 직계 선조이다.
- x86-32 전용이다. 1·2바이트 opcode 표로 길이를 풀고, 모르는 opcode는 `0xCE`로 이스케이프한다. 출력은 버퍼 20개이다.
  - opcode, ModRM rm별 disp8(8개), rel8, imm8/16/32, disp32 둘, 절대 주소와 점프 표, call 인덱스, 점프 델타, 새 call 타깃, SIB이다. disp32와 imm은 바이트를 뒤집어 저장한다.
- call: 255칸 함수 시작 주소 링 표(MTF가 아니라 선형 검색)를 둔다. 표는 `ret`/`int3` 다음 명령과 새 call 타깃으로 채운다. 표에 있으면 인덱스 1바이트, 없으면 0과 절대 주소를 쓴다.
- **`E9`/Jcc rel32는 절대 타깃을 앞 점프 타깃과의 차이로 바꿔 zigzag로 저장한다.** aqr-exe는 절대 타깃을 그대로 저장한다.
- 점프 표: `FF 24`(`jmp [reg*4+disp32]`)의 disp32 최솟값을 기억했다가, 그 주소에 오면 개수와 엔트리를 따로 담는다.

### Crinkler, paq8px [분석]

- Crinkler(https://github.com/runestubbe/Crinkler, zlib):
  - `source/Crinkler/CallTransform.cpp`는 `E8` 변위가 ±32 K일 때만 16비트 절대로 바꾸는 가장 단순한 변환이다.
  - 압축기는 "앞 바이트 마스크" 문맥의 바이트 단위 문맥 혼합이다. 모델 마스크와 가중치는 압축 때 골라 디컴프레서에 같이 싣는다. 링크 단계에서 섹션 조각 순서를 경험적으로 정렬한다(`EmpiricalHunkSorter`).
  - 4/64 KB 인트로가 목적이다.
- paq8px(https://github.com/hxim/paq8px, GPL-2.0 이상):
  - `src/filter/exe.hpp`는 64 KiB 블록별 고전 E8E9 변환이다.
  - `src/model/ExeModel.cpp`(약 1,100줄)는 DisFilter에서 출발한 **문맥 모델**이다. 입력을 x86/x64로 파싱한 상태(prefix, opcode, ModRM, SIB)를 해시 문맥 10여 개로 쓴다.

### Courgette, Zucchini [분석, 일부 개요]

- Courgette는 Chromium main에서 사라졌다(`chromium/src/courgette`가 404). 디스어셈블 후 주소를 라벨로 바꿔 diff하는 패치 생성기였다(기억에 의존한 설명).
- 후속 Zucchini(`components/zucchini/`, BSD)는 ELF32/64의 x86, x64, ARM32, AArch64를 받는다.
  - 참조 풀이 셋이다. 재배치 섹션의 `r_offset`(kReloc), 재배치 위치의 abs32, 코드의 rel32이다.
  - x86 rel32는 abs32 위치를 뺀 틈에서만 휴리스틱 디스어셈블로 찾는다(`rel32_finder.cc`: `E8`/`E9`, `0F 8x`, RIP 상대 `FF 15`/`FF 25`/`89`/`8B`/`8D`).
  - ARM은 명령 형식별로 직접 풀고, Thumb2는 AL 조건 빈도로 판정한다.
  - 타깃 풀을 정렬해 키로 바꾼다.

### ELF 재배치 포맷 [분석]

- CREL(LLVM 19 이후, Fangrui Song, `SHT_CREL`, 디코더는 `llvm/include/llvm/Object/ELF.h`의 `decodeCrel`):
  - 헤더는 ULEB128 하나(`count·8 | addend 플래그 | shift`)이다. 항목마다 오프셋 델타와 플래그 바이트가 오고, symidx·type·addend는 앞 항목 대비 SLEB128 델타이다.
  - LLVM 토론의 수치로는 clang 릴리스 빌드에서 `.rela.*`가 파일 크기의 약 21%였고, CREL로 x86-64와 aarch64가 18.0%, riscv64가 34.3% 줄었다. 재배치 가능 목적 파일(.o)의 수치로 보인다. **링크된 실행 파일과 .so에는 `.rela.dyn`만 남으므로 그대로 적용되지 않는다.**
- RELR(`SHT_RELR`): 정렬한 상대 재배치를 주소와 비트맵(이어지는 워드 63개)으로 적는다.
- Android APS2(lld `AndroidPackedRelocationSection`): SLEB128 그룹 부호화이다. Chromium DSO의 재배치가 2,911,184 B에서 174,693 B(6%)가 되었다.
- 둘 다 이미 압축된 형태라 더 줄일 것이 적다. 지금 상태 그대로 두거나 바이트 구조만 나누면 된다.
- "Extreme HCR on ELF"(3976)는 encode.su 밖의 자료를 찾지 못했다.

### .NET 압축기 [개요]

- NETZ, Costura.Fody, ConfuserEx의 압축기, .NET 6 이후 단일 파일 번들의 압축은 모두 어셈블리를 통째로 deflate나 LZMA로 압축하는 것으로 안다(미검증). IL과 메타데이터 구조를 쓰는 것은 없다.

### Aqueous에 대한 시사점

- **ELF는 PE·Mach-O와 같은 틀로 넣을 수 있다.** 필요한 것은 실행 섹션(`SHT_PROGBITS` + `SHF_EXECINSTR`), 프로그램 헤더(VA 매핑), `e_machine`이고, 필터는 그대로 쓴다.
  - `.plt`와 `.plt.sec` 스텁의 RIP 상대 disp32는 `.got.plt`를 가리키므로 `ADDR64`로 그대로 이득을 본다.
  - ELF에서는 call이 PLT로 몰리므로 call 캐시 적중률이 PE와 다를 수 있다.
  - Zucchini처럼 `.text` 안의 포인터 재배치 위치를 코드 스캔에서 빼는 것이 안전하다.
- **ELF의 코드 밖 구조**(기존 도구는 코드 필터와 범용 압축뿐이다):
  - `.rela.dyn`: `Elf64_Rela`를 오프셋 델타, type, symbol, addend 열로 나눈다. `R_*_RELATIVE`는 오프셋 델타가 대개 8이고, addend는 내부 포인터이다.
  - `.dynsym`, `.symtab`: aqr-exe의 .NET 표 처리(열 분리, 정렬 열의 델타)를 그대로 쓴다. 문자열은 접두사를 공유한다.
  - `.gnu.hash`, `.hash`: 이름에서 다시 계산할 수 있다.
  - `.eh_frame_hdr`의 정렬 표는 FDE에서 완전히 다시 만들 수 있다. FDE의 `pc_begin`과 `pc_range`는 함수 경계와 연결하면 거의 0비트가 된다(구현이 어렵다).
- **PIC 점프 표.** gcc/clang PIC의 `lea table(%rip); movslq (table,idx,4); add; jmp *` 표는 `.rodata`에 표 기준 32비트 상대 오프셋으로 있다. aqr-exe의 x86-64 점프 표는 절대 `le64` 엔트리만 알아보므로(FORMAT.md §6), ELF와 Mach-O에서는 이 꼴을 알아보아 절대 주소로 바꾸는 것이 이득일 수 있다(실측 필요).
- **점프 타깃 부호화 비교.** kkrunchy는 앞 점프 타깃과의 차이(zigzag)로, aqr-exe는 절대 타깃으로 저장한다. 함수 안의 지역성을 생각하면 델타가 작을 수 있고, 절대는 반복 일치에 유리하다. 같은 코퍼스에서 비교해 볼 만하다.
- **ARM64의 `ADRP` 짝.** aqr-exe도 xz처럼 `ADRP`만 절대로 바꾼다. 바로 뒤나 가까이에서 같은 레지스터를 쓰는 `ADD`/`LDR`/`STR`의 lo12를 묶어 전체 주소로 만들면, 같은 전역 변수를 가리키는 참조가 한 값으로 겹친다. xz RISC-V의 `AUIPC` 쌍과 같은 발상이고, ARM64 공개 구현은 찾지 못했다. Linux aarch64 PLT 스텁(`adrp/ldr/add/br`)도 이 꼴이다.
- **RISC-V.**
  - 명령 길이는 하위 비트로 정해져(RVC 2바이트, 기본 4바이트) x86보다 파싱이 훨씬 쉽다. xz의 선택(`JAL`은 x1/x5만, `AUIPC` + 다음 명령 쌍, 조건 분기와 `C.J`는 제외)이 출발점이다.
  - 스트림을 나누면 xz가 포기한 부분까지 할 수 있다. S-type의 imm 위치를 바로 다루고, 레지스터별 마지막 `AUIPC`를 짧은 창에서 추적하면 떨어진 쌍도 잡는다. 오탐은 왕복 검증이 거르므로 xz보다 과감해도 안전하다.
  - 링커 relaxation이 호출을 `jal`로 줄이므로, 실제 바이너리에서 비율을 먼저 잰다(Debian riscv64의 `/usr/bin`, `libc.so`를 RVC 유무로 나누어).
- **리터럴 풀 오인**(README의 ARM64 한계): xz의 ±512 MiB 범위 검사나 "바꿔서 커지면 되돌리기"를 응용할 수 있다.
- aqr-exe가 앞선 점: x86은 kkrunchy를 넓힌 구조(46 스트림, x86-64, VEX/EVEX/REX2, MTF call 캐시, 완전한 명세)이고 공개 도구 중 비교 대상이 없다. ARM64는 xz보다 낫다(−17.6% 대 −12.1%). CIL은 비교 대상이 없다.


## 9. 아카이브 포맷과 VM

### ZPAQ [분석]

- https://github.com/zpaq/zpaq(미러), 7.15(2016-08)에서 동결. public domain(divsufsort 부분은 MIT).
- 포맷(`libzpaq.h`, `libzpaq.cpp`):
  - 블록 헤더에 압축기 전체가 들어간다. level, 컴포넌트 목록(CM, ICM, MATCH, AVG, MIX2, MIX, ISSE, SSE), 문맥 계산용 HCOMP 바이트코드, 후처리용 PCOMP 바이트코드이다.
  - **컴포넌트는 고정된 네이티브 구현이고, VM은 문맥 계산과 후처리만 한다.**
  - ZPAQL 상태는 A, B, C, D, R0–R255(32비트), F, H[2^h], M[2^m], 16비트 PC이다. PCOMP 코드는 첫 세그먼트에 압축되어 실린다.
- 저널링 층(`zpaq.cpp`): content-defined 조각(기본 평균 64 KB)마다 SHA-1을 두어 조각 단위로 중복을 제거한다. append-only이고, 버전은 트랜잭션 날짜이며 과거 시점으로 되돌릴 수 있다.
- 안전과 결정성: 모든 메모리 접근을 마스크하고, 점프 목표를 검사하고, 모르는 opcode는 오류로 처리한다. 정수 연산만 쓴다. **명령 수나 시간 제한은 없고**(HCOMP 무한 루프 가능), 메모리는 헤더가 2^32까지 선언할 수 있다. 버전은 level 바이트(1, 2)와 ZPAQL 종류(1)뿐이다.
- JIT는 x86-32/64 전용이다(`libzpaq.cpp:3035–4585`, RWX 메모리). 그 밖의 아키텍처는 `-DNOJIT`가 필요하다.
- 블록마다 헤더 바이트코드를 다시 읽는다. 해시로 식별하거나 블록 간에 공유하지 않고, 같은 헤더가 블록마다 반복된다.

### zpaqfranz [분석]

- https://github.com/fcorbelli/zpaqfranz, MIT, 매우 활발. `zpaqfranz.cpp` 하나가 약 26만 줄이다.
- **포맷은 바꾸지 않는다.** README가 "목록이 느린 것은 포맷 구조 탓이지만 호환성 때문에 못 고친다"고 하고 `-filelist`로 우회한다. 변경은 도구 쪽이다(SHA-1·XXH64·CRC-32 삼중 검증, 병렬 테스트, SFX, 원격 저장, 실험적 암호화).
- JIT 교훈: ARM과 Apple에서는 `-DNOJIT`가 "FAQ 1번"이다. W^X OS(OpenBSD)에서는 RWX가 거부되어 인터프리터로 돌았다. 65.3o에서 쓰기 후 `mprotect`의 2단계로 바꿔 `-m5` 추출이 93초에서 56초가 되었다(`zpaqfranz.cpp:1593–1606`, 저자 주장).

### OpenZL [분석]

- https://github.com/facebook/openzl, BSD. 2025-10의 0.1.0 이후 활발하다(0.3.0이 2026-09). 프레임 포맷 버전이 아직 오르는 중이다(최소 8, 최대 27, `include/openzl/zl_version.h`). `wire_format.h`도 "확정되지 않았다"고 적는다.
- **압축 때 해소한 디코딩 DAG를 프레임 헤더의 Decoding Map으로 직렬화한다**(`src/openzl/common/wire_format.h`).
  - 디코더마다 표준/커스텀 플래그, 디코더 ID, private header 크기, 입력과 출력 수, 출력 스트림까지의 거리를 담는다. 이 표를 필드별로 전치해 비트 패킹과 varint로 줄인다.
  - 청크마다 새 Decoding Map을 둘 수 있다.
- **코덱은 ID로 가리키는 고정 변환이고 라이브러리에 내장되어 있다.** 커스텀 코덱은 디코더에 등록해야 하며 문서가 권하지 않는다(`concepts.md`). 코덱과 노드가 "최소 라이브러리 버전"을 선언한다(0.2.0). 따라서 "범용 디코더"는 고정된 코덱 카탈로그 위에서의 범용이다.
- SDDL(데이터 기술 언어, SDDL2는 스택 VM `compress/graphs/sddl2/sddl2_vm.c`)은 **압축기 전용**이다. `decompress/`에는 없다.
- 학습(`zli train --profile csv`)은 모델 내용이 아니라 그래프와 파라미터 선택을 학습한다(CSV에서 10%라고 주장). 사전은 프레임이 ID(최대 32 B)로 가리킨다.
- 한도: 스트림·노드 수에 런타임 상한이 있다. 문서(`library-limitations.md`)가 메모리는 페이로드의 약 10배, 스트리밍 미지원, 500 MB 초과는 미정의라고 밝힌다.
- 수치(문서 주장): SAO에서 비율 2.06(zstd -3 1.31, xz -9 1.64), 압축 203 MB/s, 해제 822 MB/s. 디코더가 부모와 자식 코덱 한 겹을 융합한다(`decompress/decoder_fusion.c`).

### paq8pxv [분석]

- https://github.com/kaitz/paq8pxv, GPL-2.0, 마지막 커밋 2024-10.
- 모델(`.cfg`), 디코드 변환(`.dec`), 검출(`.det`)이 **C 비슷한 소스**이다. 이 소스를 압축해 아카이브에 싣고, 디코더가 자체 c4 계열 컴파일러(`vm.cpp`, 2,174줄)로 바이트코드로 컴파일한다. 즉 컴파일러의 동작이 사실상 규범이 된다.
- ZPAQ처럼 컴포넌트(20종)는 네이티브이고 VM은 개수, 초기화, 문맥만 정한다.
- lpaq1과 같은 모델을 옮기면 enwik8이 123초(네이티브) 대 476초(JIT)로 약 3.9배 느리다(README).
- 경계 검사는 옵션이다. JIT는 RWX `mmap`이고 32비트 포인터 가정이 있다. 신뢰된 설정을 전제한 것으로 보인다.

### RarVM [분석]

- unrar 7.20 beta1(미러 https://github.com/aawc/unrar)의 `rarvm.cpp`, `unpack30.cpp`와 libarchive의 BSD 구현을 읽었다. unrar 라이선스는 해제 전용이라 RAR 호환 압축기에는 쓸 수 없다.
- 사양: 메모리 256 KB, 레지스터 R0–R7, 프로그램(64 KB 미만, 앞에 XOR 체크섬 1바이트)이 필터 블록 앞에 실린다. 필터는 8,192개 이하이다.
- **현행 구현에는 일반 인터프리터가 없다.** `RarVM::Prepare`가 프로그램의 CRC32와 길이로 표준 필터 6개(E8, E8E9, ITANIUM, DELTA, RGB, AUDIO)를 알아보고 네이티브로 실행한다. 모르면 필터 없음으로 처리한다. libarchive도 같은 방식(CRC32 | 길이 << 32)이고, 나머지는 "No support for RAR VM program filter"로 실패한다.
- RAR5가 VM을 없앤 이유는 공식 technote에도 unrar 주석에도 적혀 있지 않다.

### Aqueous에 대한 시사점

- **RarVM은 xform-design §5.7을 뒷받침한다.** 해시로 식별해 네이티브로 치환하는 것이 실제 생태계에서는 사실상 유일한 실행 경로였다. 명세는 이를 SHA-512/256 모듈 해시와 registered module로 정했다(`#module-block`, `#registered-blocks`).
- **최소 보장 자원 한도.** 이제 아카이브가 적용마다 메모리 한도를 선언하지만(`apply_memory_limit`), 구현이 정한 한도가 그보다 작으면 여전히 resource failure이다(`#semantics`). 그러면 어떤 구현에서는 풀리고 다른 구현에서는 실패하는 아카이브가 생긴다. ZPAQ의 32비트 빌드와 64비트 빌드가 같은 문제를 겪었다. zstd 창의 128 MiB처럼 적합성 수준마다 하한을 두는 편이 낫다(명세 `#module-execution`의 Issue).
- **JIT 이식성.** ZPAQ 계열은 x86 RWX JIT에 묶여 ARM과 W^X에서 고생했고, paq8pxv의 JIT도 네이티브의 3.9배이다. 인터프리터를 폴백으로, 네이티브 치환을 JIT와 독립으로 둔 xform-design의 결정이 이를 이미 반영한다. 인터프리터, JIT, 네이티브의 결과가 같은지 확인하는 적합성 벡터를 규범 부록에 둘 만하다.
- **코드 공유.** ZPAQ는 압축기를 블록마다 반복해 싣고, paq8pxv는 소스를 실어 컴파일 동작이 규범이 된다. Aqueous는 Wasm 바이트코드를 규범으로 하고 해시로 식별해 한 번만 두므로 두 문제를 모두 피한다.
- **OpenZL은 가장 가까운 대응이다.**
  - 같은 점: 압축 때 해소한 그래프를 직렬화하고 디코더는 재생만 한다. 파서(SDDL)는 압축기 전용이다. xform-design §4.2와 같은 결론이다.
  - Aqueous가 다른 점: 새 변환을 Wasm으로 동봉하므로 디코더를 고칠 필요가 없다. OpenZL은 코덱 ID가 라이브러리에 묶이고 포맷도 아직 불안정하다.
  - 참고할 점: Decoding Map의 필드별 전치와 비트 패킹. 파일이 매우 많을 때 formula 블록의 헤더 비용을 재 볼 만하다. "최소 버전 선언"은 표준 모듈 목록을 버전화할 때 일찍 실패하는 신호가 된다.
  - 메모리 약 10배와 스트리밍 미지원은 범용 DAG 실행기에서 메모리 모델이 어렵다는 증거이다. Aqueous의 노드별 크기 선언, 적용마다의 메모리 한도, eval-design의 사전 계산 최고 수위는 그 점에서 강점이다.
- **조각 단위 중복 제거**(ZPAQ, 평균 64 KB CDC와 SHA-1): Aqueous는 같은 파일, 같은 블록, 그리고 다른 파일이 그대로 가진 내용(`share_slices`)을 나눠 갖는다. 큰 파일의 여러 버전 사이처럼 조금씩 다른 부분 중복은 잡지 못하고, aqr-git(델타)만 이를 다룬다(10절).
- **포맷 동결의 대가**(zpaqfranz): 목록 비용이 포맷에 박히면 고칠 수 없다. 엔트리 표를 정적 메타데이터 블록으로 둔 결정이 맞다.

### lzip의 xz 비판과 명세 [분석]

https://www.nongnu.org/lzip/xz_inadequate.html. lzip은 6바이트 헤더(매직, 버전, dict), LZMA 스트림, 고정 폭 트레일러(CRC32, 데이터 크기, 멤버 크기)이고 선택 필드와 패딩이 없다. 멤버마다 매직이 있어 재동기화되고, lziprecover가 손상된 사본 여럿을 합친다.

비판은 2026-10-25에 명세에 반영했다(최단 leb128, 패딩의 해석 무시, archival profile, critical과 non-critical 블록 종류, 층 사이 크기 일치의 MUST, 아카이브 찾기). 남은 것은 블록 헤더 CRC를 두지 않는 대신의 재동기화이다. 다음 leaf block에서 metadata의 `file_offset`으로 재동기화하는 절차를 Repair 절(recovery-design S3)에 넣는다. non-leaf block은 순번으로 참조되므로 손상 지점 뒤로 같은 parcel의 위치를 잃는다.

### 장기 보관, 재현 가능 아카이브, Wasm 성능 [개요]

- 장기 보관 지침(Library of Congress의 지속 가능성 요소 등, 기억에 의존): 공개성, 채택, 투명성, 자기 문서화, 외부 의존이 핵심이다. 디코더를 아카이브에 넣는 것은 자기 문서화에 강하지만, Wasm 엔진이라는 외부 의존이 생긴다.
- 재현 가능 아카이브(https://reproducible-builds.org/docs/archives/): 파일 순서, mtime, uid/gid를 고정한다. Aqueous는 `etime` 양자화가 있다. 인코더 출력의 결정성을 명세가 요구하는지는 확인하지 못했다.
- PAR2와 RAR 복구 레코드: recovery-design.md §1이 이미 정리했다.
- Wasm 성능:
  - Jangda 외, "Not So Fast"(USENIX ATC 2019, 브라우저): SPEC CPU에서 평균 1.45–1.55배, 최악 2.08–2.5배 느리다.
  - wasm-brotli README: 압축이 네이티브의 약 2배 느리다.
  - xform-design §5.7의 자체 측정(WAH JIT 1.45–1.69배, Cranelift 1.24–1.37배, 인터프리터 20–34배)은 이와 같은 범위이거나 낫다. zstd의 Wasm 실측은 찾지 못했다.

### SYC [개요]

- https://github.com/YadeWira/syc-ModularCompressionTool(보관됨). zstd, srep, xprecomp, zpaqfranz, lz4 같은 외부 압축기를 FreeArc식 `.ini` 체인으로 이어 `.syc`에 쓴다. solid tar, 다중 파트, AES-256-GCM과 ChaCha20-Poly1305, CRC32와 MD5를 지원하고 Windows 전용이다.
- 아카이브만으로는 풀 수 없고(외부 실행 파일에 의존) 결정성도 없다. Aqueous가 푸는 문제의 반례이다.


## 10. 중복 제거와 구조화 데이터

### 중복 제거 [분석, 일부 개요]

- srep(Bulat Ziganshin): 공개 소스 저장소를 찾지 못했다. Tridgell식 장거리 LZ로, 사전을 RAM에 두지 않고 블록 해시를 비교해 RAM의 약 10배 크기 파일까지 다룬다고 한다. "future-LZ" 방식은 압축 때 입력을 두 번 읽고, 디코더가 앞으로의 매치에 쓸 데이터를 메모리에 둔다. 같은 저자의 REP는 2 GB RAM에서 최소 512 B 매치, 최대 1 GB 거리까지 찾는다(Wikipedia).
- eXdupe: 저장소를 찾지 못했다.
- FastCDC(Rust 구현 https://github.com/nlfiedler/fastcdc-rs, MIT, `src/v2020/mod.rs`): Gear 해시와 `hash & mask == 0`의 컷, 최소 크기 이하에서 컷 건너뛰기, 평균 전후로 마스크를 바꾸는 정규화 청킹을 쓴다. 결정적이다.
- 지금 Aqueous의 중복 제거는 같은 파일(`ref`)과 같은 블록, aqr-git의 델타, 그리고 배치 안에서 다른 파일이 그대로 가진 내용이다. 마지막 것은 서로 담을 수 있는 파일을 한 배치에 모으고(`together`, CRC와 크기, 멤버, 앞 64 KiB), 스트림을 만든 뒤 slice끼리 나눠 갖는다(`Formula::share_slices`, `10gb-todo.md` 2번). 그 밖은 zstd 창(최대 128 MiB) 안에서만 잡힌다.

### 수치와 열 [분석, 일부 개요]

- ALP와 ALP-RD(https://github.com/cwida/ALP): 1024개 벡터마다 `10^e / 10^f`를 곱해 정수가 되는 double을 정수로 바꾸고, 안 되는 값은 예외 목록으로 둔다. 정수는 FFOR(FOR + 비트 패킹)로 저장하고 (e, f)는 2단계 표본 추출로 고른다. ALP-RD는 정수가 안 되는 값의 상위 비트를 사전으로 둔다. 짧은 10진수였던 double을 정수와 지수로 되돌린다는 점에서 aqr-num과 방향이 같다.
- Pcodec(https://github.com/pcodec/pcodec, `docs/format.md`): Classic, IntMult, FloatMult, FloatQuant, Dict 모드와 델타, 구간 분할, ANS를 쓴다. 모드를 포맷이 고정한다.
- BtrBlocks(https://github.com/maxi-k/btrblocks, MIT, SIGMOD 2023): RLE, 사전, FOR, PFOR, FSST, Pseudodecimal 같은 가벼운 인코딩을 이어 쓰고, 블록 표본으로 골라 범용 압축 없이 빠르게 디코딩한다(기법 목록은 논문 기억).
- Gorilla, Chimp, fpzip, zfp, TurboPFor: 이번에는 읽지 않았다.
- 모두 이진 float가 대상이고, 텍스트로 찍힌 수는 다루지 않는다. aqr-num의 "정수 + 출력 방식"은 그 빈자리를 메운다.

### 3D [분석]

- meshoptimizer(https://github.com/zeux/meshoptimizer, MIT, `src/vertexcodec.cpp`, `indexcodec.cpp`):
  - 코덱 자체는 무손실이고, 손실은 양자화와 재정렬, 필터 단계에서만 생긴다.
  - 정점 코덱은 바이트를 디인터리브한 뒤 이웃 정점과의 비트 패턴을 쓴다. 인덱스 코덱은 삼각형 순서를 지키되 삼각형을 회전할 수 있다.
  - 디코딩은 3–6 GB/s이다. 입력이 정점 캐시 최적화가 되어 있어야 잘 줄어든다.
- Draco(https://github.com/google/draco, Apache-2.0): 기본이 양자화이고, edgebreaker가 정점 순서를 바꾸므로 원본 바이트 보존과 맞지 않는다.

### Aqueous에 대한 시사점

- **장거리 중복 제거**: VM 이미지, 백업 tar, 반복 에셋처럼 zstd 창을 넘는 부분 중복은 `together`가 묶지 못하면 놓친다. 두 가지 방법이 있다.
  - REP/srep식 장거리 매치(최소 512 B–4 KB)를 앞단에 두고 (오프셋, 길이)로 바꾼다.
  - FastCDC 청크의 해시 색인을 둔다.
  - 어느 쪽이든 디코더는 복사만 하면 된다. 다만 future-LZ처럼 디코더가 미래 매치용 데이터를 쥐고 있는 방식은 메모리 선형 맞춤과 eval-design의 사전 계산 최고 수위와 충돌하므로, 거리와 메모리 상한을 정해야 한다.
  - 포맷 모델(aqr-obj, aqr-sqlite)이 보기 전에 매치를 빼면 모델의 문맥이 끊긴다. 매치를 레코드 경계에 맞추는 편이 낫다.
- aqr-num 쪽에서는 ALP의 예외 처리와 FFOR가 정수 열을 단단히 묶는 방식, Pcodec의 모드 체계(IntMult, FloatMult, Dict)가 열 모델 선택의 참고가 된다. 이진 float에서는 aqr-infer-recs가 레코드의 float 가운데 한 배율의 배수인 것을 정수로 바꾸고(`recs-floats`, ALP와 같은 방향), aqr-hdf5가 float 데이터셋을 선형 예측기로 부호화한다. Parquet은 Snappy를 푼 뒤 열 값을 BtrBlocks와 ALP식으로 모델링하는 방향이 자연스럽다.
- 3D: meshopt와 Draco로 이미 압축된 glTF는 README의 판단(이미 예측과 엔트로피 부호화됨)대로 두는 것이 맞다. 이득은 날 버퍼의 glTF/GLB, STL, PLY에 있다.
  - STL의 법선은 정점에서 계산되는 경우가 많아 잔차 0을 노릴 수 있다.
  - ASCII PLY는 aqr-num이 바로 적용된다.
  - aqr-obj의 정점·면 예측기를 텍스트 없이 다시 쓰면 된다.

## 11. 10gb 벤치마크

Matt Mahoney의 10 GB 벤치마크(http://mattmahoney.net/dc/10gb.html)는 10gb.zpaq를 풀어 쓴다. 2026-10-26에 Apple M4(10코어)에서 쟀다. 원본은 `/tmp/testdata/10gb`에 두었다. 아래의 Insighter 수치만 그쪽 주장이고, 나머지는 직접 잰 것이다.

### 10gb.zpaq [분석]

- zpaq 7.15 아카이브이다. 버전 1개(2013-07-27), 엔트리 83,437개(파일 79,431개와 디렉토리 4,006개), 조각 180,115개, 3,701,584,921 B이다.
  - 압축 방법은 목록에 남지 않는다. enwik8을 zpaq 7.15로 압축하면 `-m1`이 35.7 MB, `-m2`가 30.8 MB, `-m3`가 22.0 MB이다.
  - 모든 코어를 쓴 `zpaq x`는 86 s(116 MB/s)가 걸린다.
- 풀면 정확히 10,000,000,000 B이다. 최상위 디렉토리는 8개이다.

  | 디렉토리 | 내용 | 원본 |
  |---|---|---|
  | `hg19` | 인간 참조 유전체 FASTA | 3.24 GB |
  | `benchmarks` | enwik8/9, silesia, gimp 등 압축 코퍼스 | 2.00 GB |
  | `mingw` | gcc 4.4–4.8 배포판 여러 벌 | 1.58 GB |
  | `www.mattmahoney.net` | 사이트 미러 | 1.21 GB |
  | `2011` | 같은 사이트의 2011년 미러 | 0.73 GB |
  | `progs` | Windows 프로그램 폴더 (OpenOffice, Firefox, VLC 등) | 0.68 GB |
  | `cygwin` | Cygwin 설치본 | 0.50 GB |
  | `zeropad` | 0으로 채운 파일 하나 | 0.05 GB |

- 파일 79,431개 중 13,820개(916 MB)가 다른 파일과 바이트 단위로 같다(SHA-1). 대부분 사이트 미러 두 벌 사이의 중복이다.
- 종류별 구성. "고유"는 같은 파일을 한 번만 센 것이다.

  | 종류 | 파일 | 원본 MB | 고유 MB | 파일별 zstd -3 (고유) MB |
  |---|---|---|---|---|
  | FASTA (`hg19/*.fa`) | 93 | 3200 | 3200 | 989 |
  | PE | 3025 | 1322 | 1189 | 555 |
  | enwik8, enwik9 | 2 | 1100 | 1100 | 348 |
  | 텍스트, gettext, pyc | 58658 | 963 | 863 | 227 |
  | ZIP (JAR, ODF, EPUB 포함) | 968 | 1094 | 662 | 647 |
  | 다른 아카이브 (PPMd, ZPAQ, LZMA, 7z, RAR, HA, CAB) | 253 | 767 | 547 | 543 |
  | ar (`.a`) | 4660 | 507 | 495 | 63 |
  | JPEG | 1491 | 237 | 236 | 231 |
  | tar | 8 | 164 | 160 | 39 |
  | PNG, GIF, BMP, TIFF, PNM | 2387 | 118 | 114 | 84 |
  | PDF | 158 | 65 | 65 | 55 |
  | WAV | 52 | 54 | 54 | 49 |
  | gzip | 2572 | 9 | 8 | 8 |
  | 그 밖 | 5104 | 401 | 391 | 219 |
  | 합계 | 79431 | 10000 | 9084 | 4058 |

- 파일 크기의 분포: 16 KiB 미만이 51,144개(174 MB, 파일별 zstd로 66 MB)이고, 4 MiB 이상인 150개가 6.5 GB를 차지한다.
- 압축 파일의 내용물이 다른 곳에 그대로 있는 경우:
  - `enwik9.zip`(322.6 MB)과 `enwik8.zip`의 내용은 `benchmarks/enwik9`, `enwik8`이다. `silesia.zip`의 12개 항목은 `benchmarks/silesia/*`와 같다.
    - 이 ZIP들은 Info-ZIP 2.3(NTFS, "maximum")이 만들었다. enwik8.zip의 본문과 silesia.zip의 `xml` 항목은 flate-zoo의 `infozip:level=9`와 비트 단위로 같다. `xml`은 `gzip:level=9`와도 같다.
    - Aqueous에서의 처리(profile 6의 레코드, 에뮬레이션의 비용)는 `10gb-todo.md` 2번과 8번이다.
  - `benchmarks/gimp.tar`(78.7 MB)는 `benchmarks/gimp-2.0.0`(파일 4,143개)을 그대로 tar로 묶은 것이다.
  - `enwik8`은 `enwik9`의 앞 100 MB와 같다.
  - mingw의 `*.tar.lzma`(123개, 같은 내용을 빼면 117개, 73.1 MB, LZMA-alone, 사전 8 MiB)는 XZ Utils `-6`의 출력과 바이트가 같고, 멤버 421 MB가 모두 설치된 파일과 SHA-1이 같다.
  - `mingw/mingw480/x32-4.8.0-release-win32-sjlj-rev1.7z`(37.1 MB)를 풀면 374 MB가 `mingw480` 트리와 같다. 폴더 0(23.4 MB)은 LZMA, 폴더 1(13.6 MB)은 BCJ2와 LZMA이다.
  - `mingw_benchmark.zpaq`(88 MB)를 풀면 `mingw44`와 똑같고, `silesia.zpaq`(48 MB)는 silesia를 담는다. 둘 다 CM이다(zpaq 7.15로 한 코어에서 8.8 MB/s, 3.8 MB/s에 풀린다).
  - `enwik9.pmd`(184.0 MB)와 `enwik8.pmd`(21.4 MB)는 Shkarin의 PPMd var.J(order 10, 256 MB, restore method 1)이다. 결정적이고 소스가 공개되어 있으며, 원본이 아카이브 안에 있다.
- 줄일 수 없는 것:
  - 파일별 zstd -3으로 3% 넘게 줄지 않는 고유 파일은 1.60 GB이다. 그중 ZIP류가 613 MB, JPEG가 173 MB, PNG·GIF가 30 MB, 나머지가 785 MB이다.
  - 나머지 785 MB는 PPMd 205 MB, ZPAQ 190 MB, LZMA 72 MB, 7z 37 MB, RAR 13 MB, UPX+NSIS 설치 파일 54 MB, `benchmarks/simple/w8002`(100 MB, 난수) 등이다.
  - Insighter의 "Incompressible 633 MB"는 여기서 PPMd를 뺀 것과 비슷한 크기이다.
- 이 디렉토리를 통째로 압축한 기준값:

  | 방법 | 크기 | 비고 |
  |---|---|---|
  | `tar \| zstd -3 -T10` | 4.68 GB | 33 s |
  | `tar \| zstd -3 -T10 --long=31` | 4.17 GB | 36 s |
  | 파일별 zstd -3, 같은 파일 제외 | 4.06 GB | |
  | zpaq 7.15 (10gb.zpaq) | 3.70 GB | |
  | Insighter (2026-10-26) | 2.27 GB | |

### aqueous의 현재 위치

- `aqr`가 디렉토리를 통째로 압축한다(배치, 종류별 블록, 같은 파일 제거, 파일 사이의 공유). 2026-10-30(커밋 `ffb9393`, README)에 10gb는 2.584 GB, 압축 162 s(62 MB/s, 최대 footprint 3.5 GB), 풀기 22.6 s(443 MB/s, 2.9 GB)이다. 종류별 결과와 남은 일은 `10gb-todo.md`에 있다.
- 가장 큰 단일 항목인 FASTA는 aqr-fasta를 만들기 전에 chr21(49.1 MB, 염기 3,510만 개)에서 비교했다. aqr-fasta는 hg19 전체를 3.24 GB → 706.6 MB로 줄인다.

  | 방법 | 크기 |
  |---|---|
  | zstd -3 | 12.1 MB |
  | zstd -19 | 9.7 MB |
  | 2비트 packing | 8.8 MB |
  | 12개 차수의 해시 문맥을 로지스틱 혼합한 간단한 CM (1.78 bits/base, 단일 스레드 5 MB/s, 최적화 없음) | 7.8 MB |

  대소문자 run(9.2만 개)과 N run(33개)은 무시할 만하다.

### 종류별 버킷의 효과

고유 파일을 파일명 순으로 이어 붙여 zstd에 넣었다. 재압축기는 쓰지 않았다.

| 버킷 | 원본 | 파일별 zstd -3 | 묶어서 -3 --long | 묶어서 -19 --long | 참고 |
|---|---|---|---|---|---|
| PE 2,795개 | 1189 MB | 555 | 398 | 305 | aqr-exe를 파일별로 적용하면 472 |
| ar 4,466개 | 495 MB | 62.5 | 40.3 | 29.4 | |
| 텍스트 (enwik와 hg19 제외, 확장자순) | 681 MB | 171.5 | 122.9 | 80.0 | |

- PE에서는 묶고 정렬하는 것(−250 MB)이 x86 필터를 파일마다 거는 것(−83 MB)의 세 배이다. 버전만 다른 gcc 바이너리가 이웃하게 된다.
- 텍스트를 여러 방법으로 압축한 결과이다(10스레드).

  | 방법 | 크기 | 시간 |
  |---|---|---|
  | zstd -9 (solid) | 100.7 MB | 1.2 s |
  | zstd -12 --long | 96.5 MB | 3.4 s |
  | zstd -16 --long | 84.9 MB | 14.5 s |
  | zstd -19 --long | 80.0 MB | 37.5 s |
  | xz -6 | 78.8 MB | 21.0 s |

  - zstd와 xz의 차이는 1.5%뿐이다. 저장 계층이 zstd인 것은 병목이 아니고, 레벨과 묶음이 병목이다.
- 16 MiB 프레임으로 나누면 -9는 101.4 MB, -19는 83.2 MB이다. solid보다 4% 손해이므로, R6(파일 단위 랜덤 액세스)을 지키면서 버킷의 이득 대부분을 얻는다.
- 디렉토리별로 보아도 같다. 파일별 고유 zstd -3과 `tar | zstd -3 --long=31`을 비교하면 다음과 같다.
  - mingw: 528 → 368 MB
  - cygwin: 165 → 127 MB
  - progs: 283 → 241 MB
  - benchmarks: 819 → 678 MB

### 목표

- **주 목표**:
  - 10gb를 2.4 GB 이하로 줄인다.
  - 풀기는 200 MB/s 이상, 메모리 2 GB 이하로, 임시 파일 없이 스트리밍하며 파일 단위 랜덤 액세스를 유지한다. 지금 443 MB/s이고, 최대 footprint 2.9 GB가 넘는다.
  - 압축은 10코어 M4에서 40 MB/s 이상이다. 지금 62 MB/s이다.
  - Insighter와 비교하면, 압축률은 가깝게 하고 풀기 속도, 메모리, 랜덤 액세스에서 앞서는 자리이다.
- **확장 목표**: 2.27 GB 이하(Insighter와 동급).
  - PPMd 재현(−205 MB)은 Insighter에는 없는 몫이지만 하지 않는다. 한 코어로 약 300 s인 직렬
    재인코딩이 압축과 풀기 예산을 모두 넘는다(`10gb-todo.md` 5번).
- 크기 예산(MB, 상당 부분 추정):

  | 항목 | 예산 | 근거 |
  |---|---|---|
  | 줄일 수 없는 것 | 580 | PPMd를 뺀 785 MB에서 일부 |
  | PPMd | 205 | 재현하지 않는다(`10gb-todo.md` 5번) |
  | hg19 | 650–720 | 2비트 packing이면 720, CM이면 650. 지금 668(aqr-fasta) |
  | enwik9 | 210–250 | enwik8은 접두어 중복. 지금 혼자 234.4(aqr-wrt) |
  | 그 밖의 텍스트 | 80 | 지금 99 |
  | PE | 260–300 | 지금 277(aqr-exe, 종류별 블록, 유사도 정렬) |
  | ar | 30 | 지금 41 |
  | ZIP 잔여 (text8, OpenOffice JAR, picasa의 JPEG 등) | ~100 | |
  | JPEG | 196 | 지금 156(같은 사진의 쌍을 공유) |
  | 그 밖 | ~250 | |
  | **합계** | **약 2.45–2.65 GB** | PPMd를 그대로 둔 것 |

  지금의 값은 `10gb-todo.md`의 종류별 표(2026-10-30, `ed0869f`)이다. 내용물 단위의 공유와 텍스트 전처리까지 하면 Insighter와 동급이 된다고 봤는데, 둘을 하고도 2.584 GB이다.

### 우선순위

상태는 2026-10-30이다. 디렉토리 아카이버와 종류별 버킷, FASTA(aqr-fasta), 텍스트 전처리(aqr-wrt), tar, LZMA(aqr-lzma), 형식 없는 데이터의 레코드 표(aqr-infer-recs)는 들어갔다. 남은 것의 자세한 것은 `10gb-todo.md`이다.

1. **Info-ZIP `-9`의 레코드: −26 MB 남짓.** 큰 DEFLATE 스트림은 profile 6으로 재현하고, 레코드가 내용의 2.6%이다(`enwik9.zip` 약 26 MB). 에뮬레이션은 쪼개기가 직렬이라 남겨 두었다(`10gb-todo.md` 2번). Insighter는 이 몫을 preflate로 가져간다.
2. **PE와 텍스트의 zstd 레벨: −29 MB 남짓**(레벨 16을 PE와 텍스트 클래스에만, `10gb-todo.md` 3번). 혼자 압축한 enwik9은 234.4 MB이고 Insighter는 198–210 MB이다. 레벨을 함께 올리면 aqr-wrt의 이득이 커진다(시제품에서 zstd -19 `--long`으로 205.7 MB).
3. **ZPAQ: −136 MB(가능한지는 아직 모른다).** 모델이 블록 헤더에 실려 있어 같은 예측기에 산술 부호기만 붙이면 된다(`10gb-todo.md` 7번).
4. **풀기의 메모리.** 최대 footprint 2.9 GB가 목표(2 GB)를 넘는다.
5. **작은 항목.**
   - ar의 COFF 오브젝트: 해 보았으나 10gb에서 커져서 넣지 않았다(`10gb-todo.md` 4번, 9번).
   - 7z 컨테이너와 BCJ2(37.1 MB), `enwik8.pmd`와 `enwik9.pmd`의 공유(−21 MB), `text8.zip`(−5.5 MB)
   - GIF와 TIFF의 LZW
   - 10gb 밖에서는 Insighter가 하는 bz2, SZDD, LZX, CAB, CHM, OLE, ISO도 있다.

일부러 따라가지 않는 것:

- GB 단위의 solid와 임시 파일을 쓰는 풀기: R5, R6과 충돌하고, 16 MiB 프레임의 손해는 4%이다.
- FastLZMA2나 PPMd 백엔드: zstd -19와 xz의 차이가 1.5%이다.
- solid JPEG: 이 코퍼스에서는 이득이 없다.
- LZMA 인코더의 재현: 대신 parse를 레코드로 적는다(aqr-lzma). 레코드가 스트림의 13–18%여도 내용물이 다른 곳에 있으면 이득이다.
- PPMd의 재현(−205 MB): 위의 확장 목표와 같은 이유이다(`10gb-todo.md` 5번).

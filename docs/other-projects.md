# 다른 프로젝트 조사

encode.su 포럼에서 Aqueous와 겹치는 프로젝트를 모으고, 저장소가 있는 것은 코드를 읽어 Aqueous에 쓸 만한 것을 정리한다. 목록은 2026-10-24에 Wayback Machine에 남은 스레드 제목 3,844개(전체 약 4,560개 중 84%)에서 골랐다. 공개 저장소나 공식 배포처가 있는 것만 읽었고, 빌드하거나 돌려 보지는 않았으므로 남의 수치는 모두 그쪽 주장이다.

남긴 것은 아직 할 일과 이어지는 것뿐이다. Aqueous가 이미 앞서거나 끝낸 일에 관한 분석은 이름만 끝에 남겼고, 내용은 이 문서의 git 이력(`67c52f4`까지)에 있다. 10gb 벤치마크는 `10gb-todo.md`에 있다.

## 요약: 남은 일

명세:

- 블록 헤더가 손상된 뒤 다음 leaf block에서 재동기화하는 절차를 Repair 절(recovery-design S3)에 넣는다(9절).
- 적합성 수준마다 보장하는 메모리 한도의 하한이 없다(명세 `#module-execution`의 Issue)(9절).

크레이트:

- aqr-compress: 헤더 없는 raw deflate를 찾지 않는다(2절).
- aqr-flate: 프로파일 9(1.0 s, 1.07%)와 10(6.2 s, 0.51%) 사이가 비어 있고, 그 자리에 preflate-rs(1.7 s, 0.78%)가 있다. 후처리기(deft4j류)가 만든 스트림의 비율을 잰다(2절).
- aqr-mp3: packMP3와의 격차 가운데 남은 후보는 같은 granule 이웃 문맥과 부호 모델이다(5절).
- aqr-vorbis: FSB5 안의 패킷만 받는다. Ogg 페이지를 재구성하면 `.ogg`가 열린다(5절).
- aqr-lz4: HC와 legacy frame을 넣으면 커널, initramfs, squashfs가 열린다(3절).
- aqr-raw: PCM 경로에서 샘플 도메인 LMS 캐스케이드와 Sac식 편향 보정, 래스터에서 이미지별 색 변환 선택을 시험한다(5절, 6절).
- aqr-flac: 가변 블록 크기 표본으로 시험한다(flacoutcpp, flake)(5절).
- aqr-exe: ELF, RISC-V, ARM64의 `ADRP`와 lo12 묶기, PIC 점프 표, 점프 타깃의 델타 부호화 비교(8절).
- aqr-pdf: 빈 사용자 암호의 암호화 파일은 공개 도구 중 하는 것이 없다(7절).

새 포맷 후보: ALAC, CD 이미지(ECM식), zstd 레벨 1–4, LZF, BC7 텍스처, camera RAW. GIF는 aqr-gif가 한다.

## 1. 범용 재압축기와 아카이버

- **xtool**(https://github.com/Razor12911/xtool, MIT, Delphi, 게임 리패커용):
  - 스트림 DB: 체크섬에서 성공한 코덱과 옵션으로 가는 표로, 맞으면 파라미터 탐색을 건너뛴다. aqr-flate의 "앞 스트림의 파라미터 재사용"을 실행 간에 유지하는 형태가 된다. 같은 도구가 만든 PDF·ZIP·게임 에셋에서 압축 시간을 줄일 수 있다.
  - 게임 엔진 컨테이너(Unreal, Unity, Frostbite 등)를 INI 플러그인으로 선언적으로 서술한다(필드의 오프셋, 크기, 식, 조건). Aqueous는 컨테이너마다 Rust 크레이트를 짜야 하니, 게임 에셋을 대상으로 삼는다면 작은 기술 언어가 필요해진다(디코드 쪽은 `slice`와 `concat`이라 인코더 전용이다).
  - 재현하지 못하면 zstd 패치가 스트림의 5% 이하일 때만 받는다. aqr-lz4나 aqr-snappy처럼 "재현 못 하면 그대로 둔다"인 크레이트에는 일반 패치 경로가 선택지이다.
  - Oodle은 게임이 가져온 DLL을 다시 부를 뿐이다(3절).
- **Fairytale**(https://github.com/schnaader/fairytale, LGPL-3.0, 2018년에 멈춤): 메모리 예산을 넘는 중간 스트림을 버리고 필요할 때 부모 변환을 다시 돌려 되살린다(`attemptRevival`). 포맷 초안은 블록마다 2바이트 체크섬을 두어 기존 아카이브에 추가할 때 빠르게 중복을 판정한다. 둘 다 Aqueous에 없다.
- **Insighter**(스레드 4541, 비공개, Zig): 10gb의 견줄 상대이다(`10gb-todo.md`). 파서로 컨테이너(TAR, ISO, ZIP, CAB, CFBF, AR, PDF, CHM, WinHelp)를 깊이 7까지 열고, 전역 중복 제거 뒤 종류별 버킷마다 REP식 장거리 중복 제거나 xwrt식 텍스트 전처리를 거쳐 FastLZMA2에 보낸다.
  - Aqueous에 없는 재압축: bzip2(어떤 도구가 만든 것이든 된다고 주장), LZW(TIFF, PDF; GIF는 aqr-gif), SZDD, MSZIP, LZX(작업 중), Base64 복원, 이미지류 데이터의 stride 인지 델타, AoS→SoA 전치.
  - 데이터를 너무 바꾸는 적응형 변환(ADPT)은 뒤단의 중복 제거를 망쳐 기본에서 껐다. 같은 내용이면 같은 출력이 나오는 변환만 버킷에 넣어야 한다는 교훈이다.

## 2. DEFLATE

aqr-flate README의 홀드아웃 벤치(807개 스트림, 40.6 MB, M4 한 스레드):

| 도구 | 정정 | 실패 | 분할 | 결합 |
|---|---|---|---|---|
| aqr-flate 프로파일 10 | 0.512% | 0 | 6.2 s | 1.6 s |
| aqr-flate 프로파일 9 | 1.069% | 0 | 1.0 s | 0.8 s |
| preflate | 2.378% | 15 | 3.8 s | 3.0 s |
| preflate-rs | 0.780% | 117 | 1.7 s | 0.8 s |

- **preflate-rs**(https://github.com/microsoft/preflate-rs, Apache-2.0, 활발): 그 사이 영역(예: 2–3 s에 0.6–0.7%)이 비어 있다. preflate-rs는 자기가 정확히 모델링하는 인코더(gzip, miniz-oxide, sdefl, zlib-cloudflare, zlib-ng)에서 aqr-flate보다 작다. 그 해시 후보와 add policy 목록이 aqr-flate "Next steps"의 에뮬레이션에 바로 참고가 된다(귀속을 지키면 코드도).
  - 해시(`hash_algorithm.rs`): zlib(memLevel별 셋), MiniZFast, Libdeflate4(4바이트와 3바이트 보조), Libdeflate4Fast, ZlibNG, Crc32c, RandomVector.
  - add policy(`add_policy_estimator.rs`): AddAll, AddFirst(n), AddFirstAndLast(n), miniz fast의 4K 경계, zlib-ng fast의 32K 경계.
- **헤더 없는 raw deflate 탐지**: aqr-compress는 컨테이너(gzip, zlib, ZIP, PNG, PDF 등)를 거쳐서만 찾는다. Precomp `-brute`(`check_inf_result`)의 거르개가 그대로 비용 모델이 된다. BTYPE=3과 stored를 버리고, 64바이트 ×4 구간의 바이트 히스토그램으로 중복이 심한 위치를 거르고, `inflate`를 2048 B만 돌려 끝났다면 1024 B 이상 풀린 것만 받으며, 실패한 위치는 기록해 다시 보지 않는다. 게임 아카이브나 독자 컨테이너에 쓸모가 있다.
- **후처리기 산물**: deft4j(https://github.com/NeRdTheNed/deft4j, 0BSD)와 JarTighten 같은 JAR·게임 에셋 최적화기, DeflOpt, defluff는 토큰을 두고 역참조를 리터럴로 바꾸고, 동적 헤더를 다시 부호화하고, 블록 종류를 바꾸고, 빈 블록과 코드 길이 끝의 0을 지운다. 그러면 토큰도 헤더도 에뮬레이션과 어긋난다. PNG와 JAR 표본에서 헤더 불일치와 블록 종류 이상의 비율을 재 보고, 많다면 이런 변형을 헤더 후보에 넣는다.
- **flate-zoo에 없는 인코더**: Intel zlib 포크(jtkukunas/zlib, CRC32 해시로 보인다; Chromium 작업과 묶음), ISA-L igzip의 asm 경로(C 경로와 다를 수 있다; 실제 빌드 표본이 필요하다). kzip, PNGOUT, turtledeflate는 최적 파싱이나 무작위화라 chain 규칙과 순위 부호화가 한계이다.

## 3. 다른 LZ와 게임 코덱

| 코덱 | 나오는 곳 | 재현 난이도 | 공개 코드 | 우선순위 |
|---|---|---|---|---|
| LZ4 HC, legacy frame, linked block | 커널, initramfs, squashfs, EROFS, Unity | 낮음–중간(`lz4hc.c` 포팅) | liblz4(BSD-2) | 높음 |
| zstd 레벨 1–4(1.5.x 한두 판) | 패키지, 커널, HTTP, OCI, 게임 | 중간(버전별 분기) | libzstd | 높음 |
| LZ4 옛 가족(1.8.1 이하) | 옛 번들 | 낮음(fixture 추가) | liblz4 이력 | 중간 |
| LZF | Redis, Krita | 낮음 | liblzf | 중간 |
| LZX(CAB, WIM, CHM) | Windows 설치·업데이트 | 높음(닫힌 인코더) | wimlib(다른 인코더) | 중간(parse 레코드) |
| GDeflate | DirectStorage 에셋 | 낮음(swizzle만) | MS(MIT) | 낮음 |
| zstd 레벨 5 이상, 멀티스레드 | 패키지 | 높음 | libzstd | 낮음 |
| LZMS, XPRESS Huffman | ESD, 프리페치 | 높음 | wimlib, ms-compress | 낮음 |
| Oodle | AAA `.pak` | 매우 높음 | 디코더뿐 | 낮음, parse 레코드는 검토 |

- **LZ4**(`lz4/NEWS`): 출력이 바뀐 것으로 읽히는 버전은 1.7.3, 1.8.1(레벨 10 이상), 1.8.2, 1.10.0(레벨 2 신설, HC 3–9 약 5%)이고, 1.9.x에는 압축률 변경 언급이 없다. 1.10.0은 `nbWorkers`와 무관하게 같은 출력을 낸다. HC는 레벨 3–9가 해시 체인, 10–12가 최적 파서이고 레벨별 파라미터는 고정 표이다. 여러 버전으로 fixture를 만들어 겹치는 가족부터 잰다. 확인할 것: 1.9.x와 1.10.0 fast의 동일성, lz4-java(Minecraft)의 출력.
- **zstd**: 거의 매 버전 압축률이 바뀌므로 기본 레벨 3의 출력도 버전마다 다르다(OpenZFS의 1.5.2→1.5.7 업그레이드로 MAC이 어긋난 사례가 있다). 반면 `ZSTD_isDeterministicBuild()`가 같은 버전을 표준 옵션으로 빌드하면 빌드끼리 출력이 같다고 보증한다. 최신 1.5.x의 레벨 1–4(fast, dfast)만 포팅하는 것이 aqr-lz4 방식의 연장이다. 바이트 같게 포팅한 공개 구현은 찾지 못했다.
- **LZX와 Oodle**: 인코더는 닫혀 있고 wimlib은 Microsoft 인코더의 재현이 아니다(저자 말로 호환되지만 바이트가 같지 않다). 비트스트림은 공개 디코더(libmspack, ooz)로 규명되어 있으니 aqr-lzma식 parse 레코드를 먼저 검토한다. 원본 바이트를 내려면 Huffman·tANS 표도 재현해야 하므로 표 구성 규칙이 결정적인지가 관건이다(미검증). CAB의 LZX는 프레임 사이에 32 KiB 상태를 이어 가므로 폴더 하나를 한 스트림으로 다뤄야 한다. ooz는 라이선스가 불분명해 클린룸이 필요하다.
- **GDeflate**(DirectStorage의 `GDeflate/`, MIT): 64 KiB 타일마다 DEFLATE 심볼을 32개 하위 스트림에 번갈아 배치한다(규칙은 README에 완결). swizzle을 풀면 보통의 DEFLATE라 aqr-flate가 받는다. 압축기는 libdeflate 포크이다.
- **LZF**(liblzf, 약 300줄): `VERY_FAST`, `ULTRA_FAST`, `HLOG`, `STRICT_ALIGN`, `INIT_HTAB` 매크로가 출력을 바꾸므로 번들마다 후보 가족을 차례로 시도해야 한다.

## 4. JPEG

aqr-jpeg는 packJPG, Lepton(lepton_jpeg_rust), brunsli보다 빠르고 작으며, paq8px는 비트 단위 CM이라 비교할 속도가 아니다. 이들에서 온 후보는 aqr-jpeg README의 "Tried and dropped"에서 모두 기각했다. GDCC 2023 과제 1의 상위 결과(0.33)는 데이터셋이 원본 75장의 다품질 사본이라 생긴 것이라 aqr-jpeg와 견줄 수 없다.

## 5. 오디오

### packMP3와 aqr-mp3

packMP3(https://github.com/packjpg/packMP3, LGPL-3.0)는 적응 산술 부호화로 고주파에서 저주파로 역순으로 계수 하나씩 코딩한다. 문맥은 같은 granule에서 이미 코딩한 상위 계수의 감쇠 평균, 같은 채널의 시간 방향 이동평균, 채널, 블록 종류, Huffman 표이다. 부호도 이력 문맥으로 모델링한다. 자체 보고 16%는 CRC, intensity stereo, mixed block, 0이 아닌 패딩 등을 거부한 파일을 뺀 모집단의 값이다.

남은 후보(코드 읽기의 가설이며 정량 검증은 하지 않았다. 시간 방향 이동평균은 이미 넣었다):

1. **같은 granule의 이미 코딩한 이웃.** aqr-mp3는 직렬 의존을 피하려고 일부러 쓰지 않는다. README의 측정으로 직전 쌍이 0.53 pp(디코딩 약 −40%), 두 쌍 전이 0.37 pp(−14%)이다. 격차의 가장 큰 몫으로 보인다.
2. **부호 모델링.** 이득은 1 pp 미만일 것이다. 비트당 이진 코딩이라 속도 비용이 크다.
3. **적응 대 정적.** 인코더와 비트레이트가 섞인 코퍼스에서는 적응이 유리할 수 있다. 정적 prior 위에 SSE/APM이나 2입력 믹싱을 얹는 혼합을 시도할 만하다.

시도 순서: (a) 채널 1에 채널 0의 인접 쌍 합, (b) 직렬 의존을 허용하는 별도 레벨(`max`)에서 역순 코딩과 부호 모델, (c) 정적 확률 뒤 SSE. 격차가 어디서 오는지는 packMP3를 `tools/compare.sh`의 FMA 402개에 돌려 계수, 부호, 스케일팩터, 부가정보로 나누어 재면 확정할 수 있다.

### Vorbis와 Opus

- balrogg(https://github.com/iczelia/balrogg, GPL-3.0이라 설계만 참고)는 `.ogg` 1,047개에서 `-1` 90.4%, `-9` 88.2%이다. aqr-vorbis는 The Crew의 FSB5에서 90.7%(큰 스트림), 93.2%(작은 스트림)이다. 표본이 달라 그대로 견줄 수는 없다.
- balrogg에 있고 aqr-vorbis에 없는 것은 residue의 채널 간 이웃 문맥, 비트 단위 혼합, match model이다. aqr-vorbis는 속도 때문에 정적 표 rANS를 쓴다.
- `.ogg`에는 Ogg 페이지와 패킷의 재구성(CRC, 페이지 경계, 체인 스트림)이 남았다. balrogg는 체인 Ogg에서 링크 간에 모델을 공유하고, 깨진 CRC나 잘못된 페이지 순서는 거부한다.
- Opus는 balrogg가 range decoder를 고쳐 심볼을 뽑고 다시 합성하는 것으로 가능함을 보였지만 이득이 3–8%라 후순위이다.

### 무손실 오디오

aqr-flac의 FLAC 경로에서 이미 잰 것(LMS 캐스케이드, 채널 간 탭, 샘플 도메인 재예측)은 그 README의 "Tried and dropped"이다.

- **PCM 경로(aqr-raw)에서 샘플 도메인 캐스케이드.** FLAC 경로에서는 FLAC의 LPC 위라 듣지 않았지만, PCM은 aqr-raw가 직접 예측하므로 Monkey's Audio식 int16 sign-sign LMS를 직렬로 쌓을 수 있다. Monkey's의 레벨별 (탭, 시프트)는 high (64, 11), extra high (256, 13)+(32, 10), insane (1024+256, 15)+(256, 13)+(16, 11)이다. Monkey's는 FLAC `--best`보다 3.8% 작다(ERK 2023, 16비트 스테레오 61개).
- **Sac식 편향 보정**(https://github.com/slmdev/sac, MIT, `pred/bias.h`): 앞 입력과 델타의 부호·크기 비트로 만든 문맥 셋마다 평균 편향을 세어 예측에 더한다. 샘플당 10사이클 안팎이다. Sac은 FLAC -8pe 대비 89%로 가장 작은 쪽이지만 fp64 내부라 출력이 플랫폼에 묶이고 매우 비대칭이다.
- **두 전문가 혼합**(Sac의 `BlendLS`): 스텝이 다른 sign-sign LMS 둘을 최근 |오차|로 섞는다. 정수 가중치 두 개로 된다.
- PCM이 L/R로 들어올 때 둘째 채널에 첫 채널의 같은 시점 샘플을 탭으로 넣는다(Sac).
- **가변 블록 크기 FLAC**: aqr-flac은 헤더 예측이 처리한다고 하지만 표본으로 시험하지 않았다. flacoutcpp(https://github.com/Snesnopic/flacoutcpp, FLACOut의 공개 재구현, 블록 크기를 DP로 탐색)와 flake로 표본을 만든다.
- **포맷 우선순위**:
  1. ALAC(https://github.com/macosforge/alac, Apache-2.0): 4096프레임 패킷, 정수 매트릭스 믹스, 차수 최대 31의 LPC, 적응 Golomb(`ag_enc.c`)이다. Rice 파라미터가 잔차 열의 순수 함수라 잔차만 알면 비트스트림이 정확히 재현되고, aqr-flac과 같은 설계가 된다. MP4 컨테이너(`stsz`, `stco`) 분해가 필요하다.
  2. Shorten(etree의 옛 라이브 녹음)
  3. TTA(인코더의 결정성을 먼저 확인)
- Monkey's, WavPack, OptimFROG, TAK는 합성까지 하려면 인코더를 비트 단위로 재현해야 해서 남는 것이 적다.

## 6. 이미지

- **GIF.** [완료] aqr-gif. Precomp(giflib 인코더를 다시 돌려 다른 곳만 적는다)와 paq8px(clear 위치 하나와 최장 일치가 아닌 위치 목록, 다르면 포기)와 달리, 화소가 주어졌을 때 코드열을 최장 일치에 견주어 부호화하므로 어떤 코드열이든 받는다. 본 인코더는 모두 최장 일치에 사전이 가득 차면 clear였다. 남은 것은 그 README의 Status이다.
  - flexiGIF 2018.11a의 최적화 옵션(clear 시점, 탐욕 대 lookahead, 첫 clear 생략, 사전 4093 제한)이 만드는 코드열도 레코드가 받지만, 최적화된 파일로 비용을 재 보지는 않았다.
- **camera RAW.** 바이트 정확 RAW 재압축기는 공개 오픈소스에서 찾지 못했다(Rawzor는 상용). aqr-jpeg의 Huffman 디코더를 SOF3로 넓히는 것이 주 작업이다. 재현할 것은 Huffman 표, 예측기 1–7, scan과 restart 구조이고, 모델은 Bayer 평면별 문맥, 2차원 잔차, black level과 비트 깊이의 상한을 쓴다.
  - 구현 순서는 rawspeed(`src/librawspeed/decompressors/`)의 디코더 목록을 지도로 삼는다. CR2와 DNG(LJpeg, deflate), NEF, ARW2, 그다음 RW2, ORF, PEF, IIQ, RAF이다. ARW2는 엔트로피 부호화가 없어 이득이 가장 크다.
  - 컨테이너는 TIFF/EXIF이므로 aqr-raw의 TIFF 파서(지금은 비압축만)를 넓혀 함께 쓴다.
- **BC7, BC6H, KTX**(aqr-tex는 BC1–BC5): BC7은 모드별 가변 비트 필드, 파티션, p-bit를 나눠야 하고, 예약 모드 같은 무효 인코딩도 고려해야 한다. 범용 LZ 위의 기준선은 BC7Prep(Oodle Texture)의 약 10%이고, BC7Prep에 특허 출원이 있으니 청구항을 확인해야 한다.
- **래스터 모델(aqr-raw).** fNBLI(GPL-3.0, 실행 파일만)는 CLIC2021 585장에서 PNG 대비 −29.8%를 압축 94.8 MB/s, 해제 149 MB/s(한 스레드)로 낸다. 약 100 MB/s 목표가 비현실적이지 않다는 근거이다. QLIC(https://github.com/OzzyMitchell/QLIC, Apache-2.0)과 같은 계열인 aqr-raw에서 값싸게 더 얻을 후보:
  - 이미지마다 색 변환을 고른다(G 델타, B 델타, 가중 블렌드 등). 인코더 시간만 들고 디코더는 한 번만 분기한다.
  - 클램프 그래디언트와 MED 중 하나를 인코더가 고른다.
  - 시험 인코딩으로 고를 때는 QLIC처럼 "디코딩을 30% 늦추면서 1%를 얻는 모드는 뺀다" 같은 규칙도 같이 둔다.
  - JPEG XL의 Weighted 예측기는 100 MB/s에서는 SIMD 없이는 비싸다.

## 7. PDF, 디스크 이미지, 비디오

- **aqr-pdf의 암호화 파일**: 빈 사용자 암호의 파일을 복호화해 재압축하고 다시 암호화하는 공개 도구는 없다. RC4와 AES-CBC(저장된 IV)의 재암호화는 결정적이라 구현이 단순하다. 나머지 열린 항목은 aqr-pdf README의 Next steps이다.
- **CD 이미지(ECM식)**: 수요(Redump 덤프)에 비해 구현 비용이 낮다.
  - ECM(Neill Corlett, GPL이므로 코드는 가져오지 않고 ECMA-130에서 새로 작성한다)은 섹터를 다시 만들어 원본과 같을 때만 그 종류로 정하고, 복제 방지용 오류 섹터 같은 불일치는 리터럴로 둔다. Mode 1은 데이터 2048 B, Mode 2 Form 1은 서브헤더 8 B와 데이터 2048 B, Form 2는 서브헤더와 데이터 2324 B를 저장한다.
  - 재구성: sync 12 B는 고정, 주소는 섹터 번호(MSF BCD), EDC는 반사형 CRC-32 변형(다항식 0xD8018001), ECC는 GF(2^8) 위의 RSPC P 패리티(43열 × 26행)와 Q 패리티(26대각 × 43)이다. 이득은 Mode 1 섹터당 304/2352 B, 약 13%이다.
  - 더할 것: MSF 주소 예측, Mode 2 서브헤더 8 B 중 중복된 4 B, 2336 B 섹터와 서브코드(96 B/프레임, Q 채널 CRC-16). CD-DA 트랙은 aqr-raw의 PCM 경로로, 데이터 트랙의 ISO 9660은 파일 단위 재귀로 넘긴다.
  - CHD, CSO, ZSO는 컨테이너로 풀어 같은 경로로 보낸다. CHD(MAME, BSD-3-Clause)의 CD 코덱은 프레임마다 재생성할 수 있으면 sync와 ECC를 빼는 비트맵을 두는 것으로 안다. WIM/ESD는 이미 SHA-1로 중복 제거되어 있고 LZX 재현이 전제라 후순위이다.
- **비디오**: 공개된 무손실 H.264 재압축기나 MJPEG 중복 제거는 찾지 못했다. 선례 없는 자체 설계가 되고, MJPEG는 aqr-jpeg를 다시 쓰면 되므로 가장 싸다.

## 8. 실행 파일

- **ELF**는 PE·Mach-O와 같은 틀로 넣을 수 있다. 필요한 것은 실행 섹션(`SHT_PROGBITS` + `SHF_EXECINSTR`), 프로그램 헤더(VA 매핑), `e_machine`이고, 필터는 그대로 쓴다.
  - `.plt`와 `.plt.sec` 스텁의 RIP 상대 disp32는 `.got.plt`를 가리키므로 그대로 이득을 본다. call이 PLT로 몰리므로 call 캐시 적중률이 PE와 다를 수 있다.
  - Zucchini(Chromium `components/zucchini/`, BSD)처럼 `.text` 안의 포인터 재배치 위치를 코드 스캔에서 빼는 것이 안전하다.
  - 코드 밖: `.rela.dyn`은 오프셋 델타, type, symbol, addend 열로 나눈다(`R_*_RELATIVE`는 오프셋 델타가 대개 8). `.dynsym`과 `.symtab`은 aqr-exe의 .NET 표 처리나 `recs-fields`를 쓴다. `.gnu.hash`와 `.hash`는 이름에서, `.eh_frame_hdr`의 정렬 표는 FDE에서 다시 계산할 수 있다. RELR과 Android APS2(lld)는 이미 압축된 형태라 더 줄일 것이 적다. CREL(LLVM 19 이후)은 재배치 가능 목적 파일의 것이라 링크된 실행 파일에는 해당하지 않는다.
- **PIC 점프 표.** gcc/clang PIC의 `lea table(%rip); movslq (table,idx,4); add; jmp *` 표는 `.rodata`에 표 기준 32비트 상대 오프셋으로 있다. aqr-exe의 x86-64 점프 표는 절대 `le64` 엔트리만 알아보므로(FORMAT.md §6), ELF와 Mach-O에서는 이 꼴을 알아보아 절대 주소로 바꾸는 것이 이득일 수 있다(실측 필요).
- **점프 타깃 부호화.** kkrunchy의 disfilter(aqr-exe disfilter의 선조)는 `E9`/Jcc rel32의 절대 타깃을 앞 점프 타깃과의 차이(zigzag)로 저장하고, aqr-exe는 절대 타깃을 그대로 저장한다. 함수 안의 지역성으로는 델타가 작을 수 있고, 절대는 반복 일치에 유리하다. 같은 코퍼스에서 비교해 볼 만하다.
- **ARM64의 `ADRP` 짝.** aqr-exe도 xz(`arm64.c`)처럼 `ADRP`만 절대로 바꾼다. 가까이에서 같은 레지스터를 쓰는 `ADD`/`LDR`/`STR`의 lo12를 묶어 전체 주소로 만들면, 같은 전역 변수를 가리키는 참조가 한 값으로 겹친다. xz RISC-V의 `AUIPC` 쌍과 같은 발상이고, ARM64 공개 구현은 찾지 못했다. Linux aarch64 PLT 스텁(`adrp/ldr/add/br`)도 이 꼴이다.
- **리터럴 풀 오인**(aqr-exe의 ARM64 한계): xz의 `ADRP` ±512 MiB 범위 검사나 "바꿔서 커지면 되돌리기"를 응용할 수 있다.
- **RISC-V.** xz `riscv.c`(5.6 이후, 주석이 설계 문서 역할을 한다)가 출발점이다.
  - `JAL`은 rd가 x1/x5일 때만 바꾼다. `JAL x0`, `C.J`, `C.JAL`, 조건 분기는 절대 주소가 거의 한 번만 나와 손해라 두지 않는다.
  - `AUIPC rd≠x0`와 바로 다음 32비트 명령(`rs1 == AUIPC.rd`)의 쌍을 통째로 절대로 바꾼다. 다음 명령을 늘 I-type으로 다뤄 store(S-type)는 부정확하다. `AUIPC` 단독과 `AUIPC x0`(LPAD)은 뺀다. 2024년 기준 `AUIPC`의 99%가 연속 쌍이고, clang 16이 떼어 놓는 쌍은 놓친다.
  - 스트림을 나누면 xz가 포기한 부분까지 할 수 있다. S-type의 imm 위치를 바로 다루고, 레지스터별 마지막 `AUIPC`를 짧은 창에서 추적하면 떨어진 쌍도 잡는다. 오탐은 왕복 검증이 거르므로 xz보다 과감해도 안전하다.
  - 명령 길이는 하위 비트로 정해져(RVC 2바이트, 기본 4바이트) x86보다 파싱이 훨씬 쉽다. 링커 relaxation이 호출을 `jal`로 줄이므로, 실제 바이너리(Debian riscv64의 `/usr/bin`, `libc.so`)에서 비율을 먼저 잰다.

## 9. 아카이브 포맷과 VM

- **최소 보장 자원 한도.** 아카이브가 적용마다 메모리 한도를 선언하지만(`apply_memory_limit`), 구현이 정한 한도가 그보다 작으면 resource failure이다. 그러면 어떤 구현에서는 풀리고 다른 구현에서는 실패하는 아카이브가 생긴다. ZPAQ의 32비트 빌드와 64비트 빌드가 같은 문제를 겪었다. zstd 창의 128 MiB처럼 적합성 수준마다 하한을 두는 편이 낫다.
- **JIT 이식성.** ZPAQ 계열은 x86 RWX JIT에 묶여 ARM과 W^X OS에서 고생했고(zpaqfranz의 FAQ 1번이 `-DNOJIT`이다), paq8pxv의 JIT도 네이티브의 3.9배 느리다. 인터프리터, JIT, 네이티브의 결과가 같은지 확인하는 적합성 벡터를 명세의 부록에 둘 만하다.
- **OpenZL**(https://github.com/facebook/openzl, BSD)은 가장 가까운 대응이다. 압축 때 해소한 디코딩 DAG를 프레임 헤더의 Decoding Map으로 직렬화하고, 파서(SDDL)는 압축기 전용이다. 다만 코덱은 ID로 가리키는 라이브러리 내장 변환이고 포맷이 아직 안정되지 않았다.
  - 참고할 점: Decoding Map을 필드별로 전치해 비트 패킹과 varint로 줄인다. 파일이 매우 많을 때 formula 블록의 헤더 비용을 재 볼 만하다. 코덱과 노드가 "최소 라이브러리 버전"을 선언하는 것은 표준 모듈 목록을 버전화할 때 일찍 실패하는 신호가 된다.
- **lzip의 xz 비판**(https://www.nongnu.org/lzip/xz_inadequate.html)은 2026-10-25에 명세에 반영했다. 블록 헤더에 CRC를 두지 않는 대신의 재동기화가 남았다. 다음 leaf block에서 metadata의 `file_offset`으로 재동기화하는 절차를 Repair 절(recovery-design S3)에 넣는다. non-leaf block은 순번으로 참조되므로 손상 지점 뒤로 같은 parcel의 위치를 잃는다.

## 10. 중복 제거와 구조화 데이터

- **장거리 중복 제거.** Aqueous는 같은 파일, 같은 블록, 배치 안에서 다른 파일이 그대로 가진 내용(`together`, `share_slices`), aqr-git의 델타를 나눠 갖는다. VM 이미지, 백업 tar, 반복 에셋처럼 조금씩 다른 부분 중복은 zstd 창(최대 128 MiB) 밖이면 놓친다. 방법은 둘이다.
  - REP/srep식 장거리 매치(최소 512 B–4 KB)를 앞단에 두고 (오프셋, 길이)로 바꾼다.
  - FastCDC(Gear 해시, 정규화 청킹; Rust 구현 https://github.com/nlfiedler/fastcdc-rs, MIT) 청크의 해시 색인을 둔다. ZPAQ의 저널링 층이 평균 64 KB 조각과 SHA-1로 이렇게 한다.
  - 어느 쪽이든 디코더는 복사만 하면 된다. 다만 srep의 future-LZ처럼 디코더가 미래 매치용 데이터를 쥐고 있는 방식은 메모리 예산과 충돌하므로 거리와 메모리 상한을 정해야 한다. 포맷 모델(aqr-obj, aqr-sqlite)이 보기 전에 매치를 빼면 모델의 문맥이 끊기므로 매치를 레코드 경계에 맞춘다.
- **수치와 열.** ALP(https://github.com/cwida/ALP)의 예외 목록과 FFOR(FOR + 비트 패킹), Pcodec(https://github.com/pcodec/pcodec)의 모드 체계(IntMult, FloatMult, Dict), BtrBlocks의 가벼운 인코딩 연쇄가 aqr-num과 aqr-infer-recs의 열 모델 선택에 참고가 된다. Parquet은 Snappy를 푼 뒤 열 값을 이런 식으로 모델링하는 방향이 자연스럽다.
- **3D.** meshopt와 Draco로 이미 압축된 glTF는 그대로 둔다. 이득은 날 버퍼의 glTF/GLB, STL, PLY에 있다. STL의 법선은 정점에서 계산되는 경우가 많아 잔차 0을 노릴 수 있고, ASCII PLY는 aqr-num이 바로 적용되며, 나머지는 aqr-obj의 정점·면 예측기를 텍스트 없이 다시 쓰면 된다.

## 이름만 남긴 것

조사했지만 Aqueous가 앞서거나 끝낸 일이라 내용을 지운 것이다.

- 범용 재압축기: AntiZ, Superduper Precompressor, SYC
- DEFLATE: preflate(C++), reflate, grittibanzli, ZREC, Zflate, zRecompress, rezop, defdb, ECT, Columbo
- LZ: LZMA 재압축기와 liblzma·7-Zip 인코더의 이력(aqr-lzma), LZO(aqr-lzo)
- JPEG: packJPG, Lepton, brunsli, paq8px JpegModel, GDCC 2023, packARC, UPGRADE_PAQJPVG, jojpeg, StuffIt, WinZip 12
- 오디오: mp3dump, MP3 repacker, oggre, ogg_rehuff, rehuff, OptiVorbis, Newpack, MPEG-4 ALS, hac
- 이미지: CryoPNG, packPNM, GraLIC, BMF, FLIF, QOI, HALIC, libjxl modular, crunch, dxt-lossless-transform, Basis Universal, BCPack, PackRAW, rawdet, dnglab, PSD·TIFF(공개 도구를 찾지 못했다)
- PDF와 디스크 이미지: Precomp의 PDF 처리, smpdf, qpdf, YBE
- 실행 파일: 7-Zip BCJ와 BCJ2, Crinkler, paq8px ExeModel, Courgette, .NET 압축기
- 아카이브와 VM: RarVM(xform-design §5.7에 반영), 장기 보관 지침, 재현 가능 아카이브, Wasm 성능 연구(xform-design §5.7의 자체 측정), 압축기의 Wasm 컴파일, 포맷 설계 토론 스레드
- 중복 제거와 수치: eXdupe, Gorilla, Chimp, fpzip, zfp, TurboPFor

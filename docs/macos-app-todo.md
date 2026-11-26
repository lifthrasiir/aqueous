# macOS 앱 번들: 남은 일

macOS 앱(`Foo.app` 꼴의 디렉토리)을 `aqr`로 압축한 결과와, 최적에 가까워지려면 더 필요한 도구를 적는다.
2026-10-30에 이 Mac의 `/Applications`에서 고른 앱 7개로 재었다. 끝낸 일의 경과는 git 이력에 있고, 구현은 각 절의
포인터(aqr-exe의 FORMAT.md/README, aqr-bvx, aqr-asar)에 있다.

## 측정

Apple M4, 커밋 `f2e1d44`의 `cargo aqr -c -v`(level mid, 나머지 기본값)로 잰 **기준값**이다(아래 1·2·3·4·7절의 개선 전).
개선 뒤의 수치는 각 절에 있다(7개 앱 합계는 3절 뒤 1053.10 MB, 포인터 표 뒤 1039.19 MB, aqr-exe README의 Results).

| 앱 | 성격 | 원본 MB | `tar \| zstd -3` MB | aqr MB | aqr / 원본 | aqr / zstd |
|---|---|---|---|---|---|---|
| Secretive | Swift, 3 아키텍처(x86_64, arm64, arm64e) | 35.6 | 6.46 | 3.23 | 0.091 | 0.500 |
| Deskflow | Qt, arm64만 | 67.8 | 26.19 | 19.81 | 0.292 | 0.756 |
| Windows App | Microsoft, universal | 270.3 | 131.83 | 111.06 | 0.411 | 0.842 |
| Discord | Electron 42 | 503.2 | 205.04 | 153.88 | 0.306 | 0.750 |
| Pages | Apple iWork | 686.1 | 276.41 | 202.16 | 0.295 | 0.731 |
| Blender | arm64, Python 내장 | 934.5 | 295.43 | 242.65 | 0.260 | 0.821 |
| Google Chrome | 프레임워크 버전 3벌 | 2280.0 | 1055.17 | 607.38 | 0.266 | 0.576 |
| **합계** | | **4777.5** | **1996.5** | **1340.2** | **0.281** | **0.671** |

- 원본은 번들의 일반 파일 크기의 합이다(symlink 제외, 같은 내용 파일 포함). Chrome 번들에는
  `Google Chrome Framework.framework/Versions/`의 156.0.8078.47, .49, .76이 모두 있다(각 526 MB, universal).
- `-v`의 파일별 바이트는 solid 블록의 평균 비율로 매긴 것이라 작은 범주의 수치는 어림이다.

## 종류별

aqr 결과에서 Mach-O 파일이 75%(783 MB)이다. 나머지는 Pages의 JPEG 64 MB, 분류되지 않은 파일(대부분 Blender의 정적
라이브러리, WOFF2, gettext 카탈로그) 61 MB, Chromium `.pak` 53 MB, Pages의 binary plist 18 MB 순이다.
Mach-O 내부(같은 내용 파일 제외, 원본 2829 MB)의 zstd -3 비중(기준값 시점):

| 부분 | 원본 MB | zstd -3 MB | 비중 |
|---|---|---|---|
| 코드(arm64) | 1128.2 | 465.2 | 41.7% |
| 코드(x86-64) | 897.3 | 406.4 | 36.4% |
| `__TEXT,__const` | 221.4 | 124.6 | 11.2% |
| 코드 서명의 페이지 해시 | 23.3 | 23.2 | 2.1% (유도함, 3절) |
| `__unwind_info`, `__eh_frame` 등 | 41.8 | 21.8 | 2.0% |
| `__cstring` 등 | 80.2 | 18.2 | 1.6% |
| `__DATA*,__const` | 100.9 | 15.7 | 1.4% |
| 기호 테이블(strtab, symtab) | 210.7 | 18.0 | 1.6% (7절) |

기호 테이블은 앱에 따라 크게 다르다(strip되지 않은 Secretive와 Windows App에서 Mach-O의 18-29%, Chrome과 Discord에서는
거의 0). `__TEXT,__const`의 큰 몫은 Blender의 `libOpenImageDenoise_core.dylib`(46.5 MB, 신경망 가중치)와 Chrome
프레임워크이다. universal 바이너리의 슬라이스 사이 공유는 문제가 아니다(aqr이 fat 파일 안에서 이미 잡는다: 슬라이스를
따로 압축한 것보다 Windows App −9.8%, Electron Framework −7.1%).

## 할 일

효과가 큰 순서였다. 1, 2, 3, 7은 했고 남은 것만 적는다.

### 1. 큰 스트림을 파일 사이에서 나누기

**[구현]** `merge_plan`(`aqr-formula/src/pieces.rs`, 호출은 `Formula::merge_classes_within`): 한 클래스에 `-b`의 절반보다
긴 스트림이 k개(최대 4) 있으면 `-b`/k의 고정 조각으로 자르고, 앵커(64바이트 gear hash의 상위 12비트가 0인 위치)를
공유하는 비율이 큰 짝부터 서로 다른 스트림의 조각을 k개까지 묶는다(큰 쪽 앵커의 10% 이상). 묶음은 `-b` 안이라 B를 혼자
읽을 때 함께 푸는 것도 `-b` 안이다. 배경: 거의 같은 큰 바이너리(다른 버전)는 스트림이 `-b`보다 길어 서로 만나지 못했다.

| | 전 MB | 후 MB |
|---|---|---|
| Chrome 프레임워크 arm64 .47+.49 | 150.78 | 100.26 (−33.5%) |
| 같은 것 .47+.49+.76 | 226.93 | 141.91 (−37.5%) |
| Google Chrome 앱 | 607.38 | 410.69 (−32.4%) |
| Discord | 153.88 | 147.06 (−4.4%) |
| Windows App / Blender | 111.06 / 242.65 | 109.33 / 242.04 |
| `aqr` 자신의 빌드 셋(10-27, 10-28, 10-29) | 17.50 | 13.54 (−22.6%) |

시도한 것과 버린 것:
- 자르기만 하면 공유가 생기지 않는다(16 MiB로 자르는 비용은 +2.1%; 코드 스트림은 파일 안의 긴 문맥을 거의 안 쓴다).
  같은 위치의 조각끼리 한 스트림에 짝지어야 한다(`arm64/CODE` 두 개: 각자 124.66 MB, 8 MiB 조각끼리 78.34 MB).
- 조각 나누기: 고정 8 MiB에 닮음 짝짓기가 가장 좋다(B의 몫 Chrome .47→.49 −78%, .49→.76 −46%, Electron 42→43 −18%).
  CDC(1-8 MiB)는 더 나쁘다(−63%, −37%, −16%): 코드가 옮겨지면 내용 경계도 흔들리고 조각이 작아 문맥도 짧다. 같은 상대
  위치로 짝짓는 것은 −24~34%.
- 문턱 5%는 Chrome이 1 MB 더 줄지만 Blender가 0.4 MB 커진다(관계없는 dylib 조각이 묶임). 20%는 Chrome 421.56 MB.
- 10-29와 10-30 사이의 `aqr` 빌드는 하나도 묶이지 않았다(`rust-toolchain.toml`이 생기고 release 프로필이 fat LTO에서
  기본값으로 바뀌어 코드 배치가 통째로 달라졌다; `--patch-from`도 −5%). 메우려면 2의 함수 단위 짝짓기가 필요하다.

남은 것:
- 짝짓기의 probe: 문턱 근처의 짝을 조각 trial 압축으로 정하면 위의 1 MB 둘을 다 얻을 수 있다.
- `-b 1G`로 묶으면 오히려 커진다(Chrome 두 슬라이스 189.42 MB 대 150.78). 원인은 모른다(레벨 고르기를 볼 것).

### 2. 참조 버전의 주소 공간으로 BL·ADRP 대상을 다시 쓰기

**[구현]** `exe-recons/arm64-remap`(aqr-exe FORMAT.md §M)과 x86-64의 `x86-remap`(§N), 그 위의 `arm64-delta`(§D).
Courgette식으로 B의 분기·페이지 대상을 A의 주소로 다시 쓴다. 모듈은 A의 이미지를 입력으로 받지 않는다: 대응표의 정의역이
B의 플래그 선 명령어들이 가진 값이라 B의 formula는 혼자 풀린다. aqr-compress는 배치 안 ARM64 이미지(코드가 `-b`의
절반보다 큰 것)로 닮음(표본 함수 해시의 공유, 0.1 이상)의 최대 신장 숲을 만들고 각 이미지의 표를 부모의 것과 합성해
뿌리의 주소 공간으로 쓴다. `arm64-delta`는 뿌리가 아닌 이미지의 `CODE` 스트림을 뿌리의 것에 대해 복사 + 워드마다
예측과의 차로 둔다(B는 4 MiB 조각마다 뿌리의 16 MiB 조각 하나에 대해; `Stream::apart`, `exe::resolve`).

| 결과 | 값 |
|---|---|
| `arm64/CODE`의 `--patch-from`, Chrome .47→.49 / .49→.76 / Electron 42→43 | −75.8% / −55.6% / −19.7% |
| `aqr -c`: Chrome 프레임워크 arm64 셋 | 141.91 → 119.64 MB (−15.7%) |
| Google Chrome 앱 | 410.70 → 388.10 MB (−5.5%) |
| x86-64 remap: Chrome 프레임워크 x86-64 셋 / 앱 | 136.82 → 122.78 MB (−10.3%) / 388.01 → 373.37 MB (−3.8%) |
| arm64-delta: Chrome .47+.49 / .49+.76 / Electron 43+44 / 앱 | −3.6% / −7.0% / −12.1% / −3.9% |

함정: 같은 워드가 일정 비율 이상이기를 요구하거나 그 사이를 Myers diff로 맞추면 오히려 덜 짝지었다. 뿌리 스트림을 4 MiB
셋, 8 MiB 둘로 쪼개면 차분은 작아도 뿌리 스트림이 커져(통째보다 +6.1%, +2.9%) 아카이브가 Chrome에서 더 컸다.

남은 것:
- 배치를 넘는 짝: 이름도 크기도 다른 큰 파일 둘(앱 둘의 Electron Framework)은 한 배치에 들지 않아 짝이 지어지지 않는다
  (1의 한계이기도 하다).
- `-b`의 절반 이하인 이미지: 부모의 것과 같은 블록에 들기 어려워 플래그와 대응표만 더 든다(Blender의 `_ssl`을 `_hashlib`에
  대응시키면 11 KB 커졌다). 자식의 스트림을 부모의 것 옆에 두는 배치가 먼저이다(Chrome Helper 프레임워크 등: 0.15 MB).
- 플래그: `ADRPF`는 zstd로 52%에 머문다. 자기 자신으로 가는 값을 빼면 `BLF`가 세 배로 커져 둘 다 남겼다(0.05 MB 남짓).
- 재 보지 않은 것: 지역 분기 스트림(B0-B3 등), ADRP 다음의 `ADD`/`LDR` 하위 12비트.
- PE는 계획하지 않는다. Electron 42→43의 x86-64는 코드 자체가 달라 −2.1%뿐이다.

### 3. 코드 서명 유도

**[구현]** `exe-recons/macho-codesign`(aqr-exe FORMAT.md §G): CodeDirectory의 페이지 해시를 지우고 모듈이 파일을 다시
모은 뒤 계산해 채운다. 7개 앱 1080.60 → 1053.10 MB(−2.5%); 풀 때 파일 전체를 모아야 해서 명령어가 Pages에서 +11.5%,
최대 RSS가 0.15-0.2 GB 는다. 서명을 빼자 Deskflow의 나머지 스트림이 8 MiB 밑으로 내려가 긴 거리 일치의 문턱을 1 MiB로
내렸다(`40205d3`; 7개 앱 −0.76%, 10gb −0.11%).

남은 것:
- special slot(요구 사항, 엔타이틀먼트, `Info.plist`, `CodeResources`의 해시; CMS 블롭은 zstd 뒤 0.25 MB로 작다).
- `_CodeSignature/CodeResources`(XML plist, 번들 파일마다 SHA-1과 SHA-256의 base64): 전체로는 0.35%이지만 파일이 많은
  Pages에서는 결과의 2.3%(4.6 MB). 번들의 다른 엔트리 여럿을 입력으로 받으므로 "가변 인자 없음"과 맞게 짜야 한다
  (`git-pack-index-recons`처럼 집합으로).

### 4. LZFSE와 LZVN

**[구현]** aqr-bvx, 모듈 `bvx-recons`(구현은 그 README/FORMAT.md; 인코더의 후보 테이블을 블랙박스로 맞춰 기록의 문맥으로
쓴다). aqr-compress가 카탈로그 안의 스트림을 스캔으로 찾는다. `/Applications`의 100 KB 이상 `Assets.car` 36개(272.5 MB;
LZFSE 스트림 4,781개 236 MB와 LZVN 10,200개, 데이터 3.7 GB):

| | MB |
|---|---|
| `zstd -3` | 232.63 |
| aqr 전 | 220.47 |
| aqr 기본 레벨 | 219.06 (1.2 s) |
| aqr `-l 9` | 170.57 (74 s, CPU 574 s, 최대 8.8 GB) |

LZFSE와 LZVN은 레벨 9에서만 쓴다: 데이터가 카탈로그의 14배로 불어나(4 MB/s 남짓) 레벨 8에서 켜면 186.53 MB, 34 s이다.
LZVN만 기본 레벨에서 켜면 얻는 것이 없다(219.08 MB). 데이터를 `Kind::Pixels`로 두어 들여다보지 않으면 CPU가 236 → 174 s
이다. 풀기 6.3 s(Wasm 8.0 s). 데이터는 DEFLATE와 같이 팽창 예산(`Options::expansion`)에 든다(iMovie의 카탈로그
하나만 넘는다). zstd가 LZFSE보다 못한 카탈로그(사진, 그러데이션)에서는 스트림을 그대로 둔다.

남은 것:
- 데이터의 이미지 모델: 4바이트 픽셀(알파가 끝)이다. 렌디션의 폭과 픽셀 형식은 카탈로그의 렌디션 헤더에 있을 것이다(보지
  않았다). aqr-raw 같은 픽셀 모델이 붙을 자리이고 남은 이득의 대부분이 여기 있다. 재지 않았다.
- 파일 하나만 묶으면 모듈 블록(압축해 21 KB 남짓)이 스트림마다 따로 비용으로 잡힌다. 파일 여럿이면
  `Encoder::share_modules`로 한 번만 잡히니, 파일 안에서도 한 번만 잡히게 할 것.
- 레벨 8에서 켜려면 10 MB/s(27 s)는 되어야 하는데 지금 8 MB/s 남짓이다(CPU: aqr-bvx 28%, zstd 26%, aqr-infer-recs
  18.5%, 스필의 memmove 14%).
- decmpfs(리소스 포크나 확장 속성 안의 청크 표; `file-formats.md`, aqr-hfs의 몫), DMG의 ULFO, `.aar`의 청크, 날 LZVN 스트림.

### 5. Electron의 asar

**[구현]** aqr-asar, 모듈 `asar-recons`(`df731a62`): 헤더의 크기, 오프셋, `integrity`(파일 SHA-256)는 파일 내용에서 유도하고
헤더 JSON은 그 자리를 비운 채 둔다. 파일은 컨테이너의 내용이 되어 형식별로 재압축되고 같은 내용은 한 번만 저장된다.
asar 27개의 측정은 aqr-asar README에 있다(전체 714.42 → 697.11 MB, −2.4%). 헤더만 따로 압축하면 OpenMTP `app.asar`
(157.95 MB)에서 1.41 MB이다(aqr 결과의 6%).

남은 것(디렉토리 쪽, asar와 상관없는 aqr의 디렉토리 압축 문제): VS Code `node_modules.asar` 같은 것은 풀면 오히려
커진다(9.41 → 10.27 MB; 디렉토리의 solid 블록 합이 파일을 경로 순서로 이은 한 파일(9.73 MB)보다 크다). 확장자 먼저의
정렬(`sort_key`)이 node_modules 같은 트리에서는 경로 순서보다 2.5% 나쁘다(`-b`를 키워도 같으니 블록 상한 탓은 아니다).
solid 밖에 따로 둔 작은 파일들(WOFF, PNG, xsv 등 0.8 MB 남짓)의 몫이 큰 까닭도 볼 것.

### 6. 텍스트, 특히 JavaScript

JS는 asar 바이트의 대부분이고(Discord 85%, VS Code `node_modules.asar` 약 66%, OpenMTP 35%) asar 밖에도 있다(VS Code의
`out/`, `extensions/`에 162.5 MB, 879개, minify됨). VS Code 전체의 aqr 결과 364 MB에서 asar 밖의 JS가 30.8 MB, asar가
10.0 MB로 11% 남짓이다. 보통의 Electron 앱에서는 Electron Framework가 훨씬 커서 비중이 더 작을 것이다.

각 묶음의 JS를 경로 순서로 이은 한 파일(MB; 아직 바뀌지 않은 기준값):

| | 원본 | zstd -3 | aqr | PPMd o16(7zz) | xz -9e | aqr / xz |
|---|---|---|---|---|---|---|
| OpenMTP asar의 JS | 52.8 | 7.01 | 4.18 (`wrt-recons`) | 3.67 | **3.49** | +20% |
| VS Code `node_modules.asar`의 JS | 39.7 | 6.97 | 5.58 (`wrt-recons`) | 4.92 | **4.59** | +21% |
| VS Code의 minify된 JS | 162.5 | 42.06 | 25.26 (그대로) | 22.97 | **19.59** | +29% |

aqr이 JS에서 LZMA보다 20-29% 크다. VS Code에서 메우면 9 MB 남짓(앱의 2.5%)이고 텍스트 전반에 걸린다. 변수(아직 하나도
재지 않았다):

- 백엔드: zstd의 엔트로피 단계(리터럴이 문맥 없는 Huffman; LZMA는 lc/lp/pb 문맥, rep match, 최적 파싱 중 어느 것이 차이의
  대부분인지), aqr이 고르는 zstd 레벨·윈도우·LDM(`plan_by`, `pick`; `-l high`), `-D`(블록별 사전). 풀기 속도 목표
  200 MB/s 이상(`10gb-todo.md`) 안에서 골라야 하고 비교 상대의 풀기 속도도 같이 잴 것.
- 변환: `wrt-recons`가 minify되지 않은 JS에는 걸리고 minify된 162.5 MB에는 걸리지 않았다(이득이 없다고 판정했는지 볼 것;
  단어 사전이 JS에 맞는지). JS 전용 토큰화(식별자, 문자열, 숫자, 구두점의 스트림, minifier의 짧은 이름 규칙성; 무손실).
  source map(OpenMTP asar에 2,002개 16.6 MB, VS Code asar에 679개 6.6 MB), `.d.ts`, JSON, CSS, HTML 등 다른 텍스트.
- 배치: 파일 순서(확장자 먼저 대 경로 순서, 5절), solid 블록 상한(VS Code 트리에서는 키워도 거의 같았다), 같거나 거의 같은
  파일(node_modules의 여러 버전, 확장마다 번들된 같은 라이브러리).
- Chromium `.pak` 안의 gzip, Brotli로 압축된 JS와 HTML(9절).
- 비교 상대의 상한: brotli -q 11(큰 윈도우), 문맥 혼합류(paq8, cmix). zstd의 높은 레벨은 재지 않는다(CLAUDE.md).

### 7. 기호 테이블

**[구현]** `exe-recons/macho-symtab`(aqr-exe FORMAT.md §S): nlist를 필드별로 나누고 값은 디버그 맵의 stab과 앞의
엔트리로 예측하며, 이름은 다음 문자열, 같은 값의 이름, 뒤로의 참조로 적는다. `ld64`의 strtab은 첫 사용 순서여서 `n_strx`는
모두 유도되고 문자열은 그 순서로 front coding한다. 결과(aqr-exe README의 Results): Windows App 본체 −6.0%, 앱 −1.9%;
Secretive −9.0%, Deskflow −6.0%, Blender −2.0%.

남은 것:
- lld의 strtab: 꼬리를 공유하고(문자열 안을 가리키는 `n_strx`) 순서도 다르다. Electron Framework의 것이 슬라이스마다
  0.9 MB 남짓, WebRTC.framework(4.4-4.6 MB)는 외부 기호를 먼저 훑은 순서도, 기호 순서도 아니다.
- 남은 비용의 대부분은 문자열이다(Windows App 본체 이미지에서 front coding 뒤 zstd -7 1.13 MB, 접미사에 xz -9 0.84 MB).
  C++와 Swift의 맹글링 구조나 텍스트 쪽 백엔드(6)가 다음이다.
- 외부 기호의 값(이름 순서의 주소)이 기호 표 스트림의 3분의 1이다. 지역 기호와 stab을 먼저 풀면 이름으로 예측할 수
  있을 것이다(재지 않았다).

### 8. `deepmap2`

- `assetutil --info`로 본 `Assets.car` 렌디션의 압축은 `deepmap2`, `lzfse`, `zip`, 없음이다(코드에 `deepmap`을 다루는 것은
  없다).
- Windows App의 `Assets.car`(72.2 MB)는 거의 `deepmap2` 이미지(71.8 MB)이고 aqr로 65.9 MB에 머문다. 그 앱 결과의 59%이다.
  Pages에는 약 4 MB가 있고 다른 앱에서는 작았다.
- Apple의 비공개 코덱이라 공개 명세가 없다. 리버스 엔지니어링을 할지, CoreUI를 블랙박스 오라클로만 쓸지 먼저 정해야
  한다(LZO 때처럼 원칙이 필요하다). 그 전에 표본을 넓혀 얼마나 흔한지 본다.

### 9. 보류

- Brotli: Blender의 `Noto Sans CJK Regular.woff2`(11.4 MB, 그대로 저장), Chromium `.pak`의 Brotli 엔트리(Chrome 15 MB(버전
  3벌 합), Discord 3.9 MB). Discord의 `resources.pak` 6.65 MB는 aqr로 줄지 않는다. `file-formats.md`에서 어려운 쪽으로
  분류했다.
- 신경망 가중치: 표본에서 OIDN 하나(46.5 MB)뿐이다.
- Pages의 binary plist 117 MB(언어 디렉토리마다 374개씩)는 aqr 18.4 MB(zstd 30.3 MB)이다. 구조 모델은 측정을 더 해 보고
  정한다. `.cshelpindex` 9.5 MB는 줄지 않는다.
- Chromium `.pak`의 엔트리 표(6바이트 × 엔트리)는 Chrome 13.9 MB, Discord 4.4 MB이다.

## 보존

압축률이 아니라 충실도의 문제이다. `aqr`은 확장 속성(`xattrs`)과 리소스 포크(`forks`)를 저장하고 푼다(spec §xattrs,
§fork-streams). 따라서 `com.apple.cs.*`에 둔 Mach-O가 아닌 실행 파일의 서명과 quarantine, provenance 속성이 남는다.
남은 것: decmpfs의 투명 압축은 저장 방식이라 보존하지 않는다. 풀면 디스크를 더 쓴다.

## 부록 A. 버전 사이의 실행 파일 차분: 기존 도구와 비교

1과 2(조각 묶기와 주소 공간 다시 쓰기)를 Courgette의 후속인 Zucchini와 범용 차분 도구에 견주었다. macOS와는 상관없는
비교이지만 입력이 이 문서의 앱에서 나왔다.

### 방법

2026-11-03, Apple M4. 쌍: Chrome Framework 156.0.8078.47→.49, .49→.76, Electron Framework 43.7.7(VS Code)→44.9.3(Discord).
`lipo -thin`으로 arm64와 x86-64 슬라이스를 따로 떼어 6쌍. B의 몫: aqr은 A와 B를 한 아카이브로 묶은 크기에서 A 혼자를 뺀
것, 차분 도구는 패치 크기(Zucchini는 Firefox가 들여온 `third_party/zucchini`를 Homebrew clang으로 빌드, 패치는 `xz -9`로
셈; 모두 `-apply`로 B와 바이트까지 돌아왔다; 빌드는 저장소에 두지 않았다). Courgette는 ARM64를 모르고 ELF는 32비트만
받아 뺐다. bsdiff 4.3, xdelta3 3.2.2(`-9 -S lzma -B 1G`), zstd 1.5.7(`-7 --long=31 --patch-from`).

Zucchini는 PE와 ELF만 알고 Mach-O는 그냥 데이터로 다룬다("raw"). 그래서 Mach-O 슬라이스를 ELF64로 감쌌다
(`scripts/macho2elf4zucchini.py`: 원래 바이트를 16 KiB 뒤에 두고 ELF 헤더, 세그먼트마다 `PT_LOAD`, 섹션 헤더를 붙이고,
주소를 4 GiB 밑으로 내리며, zerofill 섹션은 뺐다; chained fixup은 옮기지 않아 Zucchini가 본 것은 PC 상대 참조뿐이다).

### 결과

B의 몫(MB). aqr 전은 1·2·3·포인터 표 이전(`511681e`), 지금은 `4ffb379`이다.

| A → B | aqr 전 | aqr 지금 | Zucchini(ELF) | Zucchini raw | bsdiff | xdelta3 | zstd `--patch-from` |
|---|---|---|---|---|---|---|---|
| Chrome .47→.49, arm64 | 12.56 | **4.48** | 3.43 | 6.94 | 6.89 | 20.46 | 53.36 |
| Chrome .49→.76, arm64 | 23.75 | **13.00** | 9.27 | 16.52 | 18.85 | 38.15 | 69.54 |
| Electron 43→44, arm64 | 49.72 | **30.77** | 19.64 | 25.70 | 32.00 | 40.70 | 56.22 |
| Chrome .47→.49, x86-64 | 8.56 | **6.01** | 4.20 | 8.29 | 8.91 | 24.62 | 58.46 |
| Chrome .49→.76, x86-64 | 24.56 | **20.88** | 10.46 | 19.80 | 23.29 | 43.76 | 75.64 |
| Electron 43→44, x86-64 | 50.78 | **48.38** | 23.64 | 31.16 | 39.88 | 48.35 | 64.01 |
| 합계 | 169.93 | **123.52** | 70.64 | 108.41 | 129.82 | 215.04 | 377.23 |

- 합계에서 bsdiff보다 작고 raw Zucchini보다 Chrome 세 쌍에서 작다. ELF로 감싼 Zucchini는 여섯 쌍 모두 작다(합계
  1.75배). x86-64는 코드 차분이 없어(서명과 포인터 표만) 덜 따라잡았다.
- 속도(압축, 한 스레드의 차분 도구 대 aqr 10 스레드): Zucchini(ELF) 103-237 s로 aqr(5.8-11.6 s)의 9-41배, bsdiff 65-128 s.
  풀기(`zucchini -apply`, `bspatch`)는 재지 않았다.
- 바이트 차분이 LZ보다 나은 것: 다시 쓴 arm64 코드 스트림에서 bsdiff는 `zstd --patch-from`의 1/8.7(Chrome .47→.49), 1/4.5,
  1/2.3(Electron)이고, x86-64에서는 덜하다(−53%, −39%, −24%; `JUMP`에서 raw Zucchini가 bsdiff보다 크게 나빠 스트림마다
  고르면 더 준다). 거의 같은 구간을 바이트 차분으로 넘기는 쪽(바뀐 변위와 상수가 0 근처로 모인다)이 LZ가 그 구간을 짧은
  매치로 끊는 것보다 낫다는 뜻으로 보인다(`arm64-delta`가 이를 가져갔다).

### 남은 것

- 코드가 아닌 부분(Chrome .47+.49에서 양쪽을 0으로 지워 본 몫): `__DATA_CONST,__const` 3.66 MB(포인터 표로 했다: 모든
  Mach-O에서 rest와 떼어 두고 트리의 이미지는 뿌리의 것에 대해 워드 차분; 7개 앱 1053.10 → 1039.19 MB, −1.3%; 2바이트
  엇갈린 삽입을 워드 단위로는 새것으로 둬서 bsdiff의 0.25 MB보다 큰 0.44 MB), `__TEXT,__unwind_info` 0.70 MB.
- 버전 사이에 조금씩 다른 `__TEXT,__const`와 `__cstring`(Electron 43→44, B 몫이 혼자 6.28과 2.37 MB, A에 대해 LZ로 2.15와
  0.15 MB). 지금은 파일의 나머지(내용 스트림)라 A의 것과 만나지 않는다. 내용을 훑어 찾는 파일(이미지 등)을 잃지 않고 A의
  것과 같은 블록에 둘 방법.
- 뿌리 스트림을 조각으로 두는 비용(통째 대비 1-2 MB): 조각 경계를 넘는 문맥을 살릴 방법.
- 차분을 부모가 아닌 뿌리에 대해 둔다. 깊은 트리(버전 여럿)에서 부모에 대해 두려면 연쇄를 풀어야 한다.
- x86-64: 코드 차분이 없다. 스트림 나눔이 버전 사이의 차분에 맞는지(Zucchini는 나누지 않은 바이트에서 더 작다). 남은
  차이가 가장 큰 곳이다(Electron x86-64, 48.38 대 23.64 MB).

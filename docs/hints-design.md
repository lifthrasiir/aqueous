# 파일에 주는 힌트 (`.aqr/attributes`)

헤더 없는 데이터, 다른 데이터 안의 스트림, 확장자가 흔한 이름(`.bin`, `.dat`)인 파일은 `detect`
(`crates/aqr-compress/src/detect.rs`)와 `kind_by_name`이 놓친다. 사용자가 경로 패턴별로 포맷과 파라미터를
알려 주는 방법이며 **[구현]**이다(`crates/aqr-compress/src/{attributes,hints}.rs`,
`crates/aqr/src/attributes.rs`, 시험 `tests/hints.rs`). 끝낸 일의 경과는 git 이력에 있다.

## 1. 원칙

- **힌트는 인코더에만 쓰인다.** 복원은 포뮬러가 전부 기술하므로 아카이브 형식(spec)에 아무것도 더하지
  않는다. 잘못된 힌트는 크기와 시간만 잃는다.
- **후보이지 단정이 아니다.** 가리킨 recompressor가 거절하면 힌트가 없었을 때와 같이 처리한다
  (`aqr-format`은 감지를 건너뛰고 먼저 시도할 뿐, 거절되면 감지로 돌아간다. 이때 이름이 말하는 포맷도 버린다).
- **트리와 함께 다닌다.** `.aqr/attributes`는 보통 파일로 아카이브에 들어가 풀었다 다시 압축해도 같은 결과가
  나온다(`.aqr/sfx/`와 같은 자리, 단 아카이브에서 읽어 되돌리는 일은 없다).
- **opt-in을 우회하지 않는다.** 내용을 보존하는 손실 정규화(`-Y`, `docs/lossy-design.md`)는 속성으로 켜지
  않는다. 아카이브 안 파일이 압축하는 사람의 선택을 바꾸면 안 된다.

## 2. 파일과 문법

### 2.1 어디서 읽는가

먼저 나온 것이 이긴다. 같은 단계 안에서는 git의 규칙(뒤의 줄, 더 깊은 디렉터리)을 따른다.

1. `-Z` 옵션(`-Z format:no-NAME` 등)
2. `--attributes FILE`(여러 번): 트리를 고칠 수 없을 때. 패턴은 입력의 루트 기준, 값이 가리키는 파일(§3.4)은
   그 파일의 디렉터리 기준
3. 입력 트리의 `.aqr/attributes`: 각 입력의 루트에 있는 것만(트리 전체의 힌트를 한 파일에서 보게 하려고)
4. 입력 트리의 `.gitattributes`: 디렉터리마다(git과 같이)
5. 감지(`detect`)와 이름(`kind_by_name`)

입력이 파일 하나이면(`aqr -c foo.bin`) 3과 4는 없고 부모 디렉터리를 거슬러 찾지도 않는다(입력 밖의 파일이
결과를 바꾸면 압축이 재현되지 않는다). `-a`는 덧붙이는 입력의 `.aqr/attributes`만 읽는다.

### 2.2 gitattributes 문법

gitattributes(5)의 문법과 의미를 그대로 따르고 파서는 직접 쓴다(gix-attributes는 gix의 다른 crate들을 끌고 온다).

- `PATTERN attr -attr !attr attr=value`. 패턴은 gitignore의 것: `/`가 없으면 어느 깊이의 기본 이름이든
  맞고, 있으면 파일이 있는 디렉터리 기준. `**`가 있고 부정(`!`)은 없다. 디렉터리 패턴은 안의 파일에
  걸리지 않으므로 `dir/**`
- `[attr]NAME …`: macro. git과 같이 `.aqr/attributes`(와 읽는 `.gitattributes`)에서 정의한다
- C 식 따옴표(`"a b.bin"`)를 쓸 수 있고 값에는 공백이 없다. 긴 값은 파일을 가리킨다(§3.4)
- 대소문자는 시스템과 관계없이 구별한다

### 2.3 컨테이너 안쪽 경로: `>`

`.aqr/attributes`의 `>`는 컨테이너 안으로 들어가는 구분자다.

```
loupe.zip>*.bin            aqr-format=tensors aqr-dtype=f16
game/*.pak>sounds/**       aqr-kind=compressed
backup.tar.zst>>db/*.dat   aqr-format=sqlite
```

- `>` 앞은 지금 트리에서의 경로, 뒤는 컨테이너 루트에서의 경로이며 각 부분이 §2.2를 따로 따른다.
  컨테이너란 aqr-compress가 이름 붙은 멤버로 나누는 모든 것(ZIP, tar, 7z, ALZ, EGG, asar, 디스크 이미지의
  파일 시스템, aqr-bundle의 packs)이다
- 이름 없는 내용 하나뿐인 것(gzip, xz, zstd)은 `>` 뒤를 비운다: `backup.tar.zst>`는 풀린 tar,
  `backup.tar.zst>>db/*.dat`은 그 안의 파일
- `>`로 들어가지 않는 패턴은 컨테이너 안의 멤버에 맞지 않는다(트리 수준의 힌트가 예상 밖의 깊이에서
  적용되지 않게). 모든 깊이에 걸려면 줄을 여럿 쓴다(`>`의 개수가 깊이)
- Unix에서 이름에 `>`가 든 파일은 `\>`. `.gitattributes`는 `>`를 구분자로 읽지 않으며(git과 뜻이 달라지므로) 그
  속성은 트리의 파일에만 걸린다

## 3. 속성

모두 `aqr-` 접두사다(git은 모르는 속성을 무시하므로 `.gitattributes`에 적어도 영향이 없다).

### 3.1 포맷: `aqr-format=NAME`

`NAME`은 `Format::from_name`의 이름(`zlib`, `png`, `safetensors`, …)이나 §3.3의 파라미터를 받는 이름이다.
Kind를 `Kind::Format(NAME)`으로 만든다(첫 바이트부터 그 포맷으로 보고 감지하지 않음). 거절되면 감지로 돌아간다는
점이 기존 `Kind::Format`("거절되면 그대로 둔다")과 달라 힌트에서 온 것은 따로 표시한다. `-aqr-format`은 감지도
하지 않는다.

### 3.2 종류와 금지

- `aqr-kind=KIND`: `Kind`의 `Display` 이름(`unknown`, `pixels`, `text`, `font`, `table`, `compressed`).
  `compressed`는 시도를 건너뛴다(암호화된 데이터, 이미 압축된 독자 포맷)
- `aqr-not=NAME,NAME`: 그 파일들에서 그 포맷들을 감지하지 않는다(`-Z format:no-NAME`의 경로별 판)

### 3.3 파라미터

헤더가 없어서 recompressor가 스스로 찾을 수 없는 것을 준다. 파라미터는 포맷마다 정의해 그 포맷의 README에
적는다. 이름은 `aqr-PARAM`이고 `aqr-format`이 맞는 파일에서만 읽으며, 모르는 값이면 경고하고 그 힌트를
버린다(감지로 돌아감). 정의된 것:

| 포맷 | 파라미터 | 뜻 |
|---|---|---|
| `tensors` | `aqr-dtype=bf16\|f16\|f32\|…` | 헤더 없는 텐서 한 개. 원소 수는 길이/원소 크기 |
| `tensors` | `aqr-layout=FILE` | 텐서들의 경계와 dtype(§3.4). TOML의 `[[tensor]]`마다 `shape`, `dtype`, `offset` |
| `raw` | `aqr-bits`(8/16/24/32), `aqr-channels`, `aqr-endian`(le/be), `aqr-sign` | 헤더 없는 PCM. 기본 16비트, 2채널, le, 8비트만 unsigned |
| 모든 포맷 | `aqr-offset=N`(10진 또는 `0x`) | 그 오프셋에서 시작하는 파일(`zlib`, 헤더 없는 `deflate` 등). 앞 바이트는 다른 데이터처럼 스캔 |

`tensors`는 aqr-tensors의 README("What is taken"), `raw`와 `deflate`는 aqr-raw와 aqr-flate의 README.
헤더 없는 것을 받는 이름(`tensors`, `pcm`, `deflate`)은 감지하지 않는 포맷이고, 레이아웃 파라미터가 있는
`aqr-format=raw`는 `pcm`이 된다.

### 3.4 무거운 데이터는 파일로

한 줄에 안 들어가는 것(텐서의 배치, 오프셋 목록)은 `.aqr/` 아래의 파일로 두고 속성이 가리킨다
(`aqr-layout=.aqr/layouts/det.toml`, 경로는 `.aqr/attributes`가 있는 루트 기준, 그 파일도 아카이브에 든다).
선택(어느 파일에)과 데이터(무엇을)를 나눈 것이다. manifest 같은 앱 고유의 파일을 aqr가 직접 읽게 하지는
않는다(스크립트가 레이아웃 TOML을 만든다).

### 3.5 git의 표준 속성 (`.gitattributes`에서만, 감지보다 약함)

`binary`/`-text`(텍스트 아님: aqr-log, aqr-xsv, aqr-wrt의 표본 추출을 건너뜀), `text`/`eol=…`(텍스트),
`filter=lfs`(작은 텍스트 포인터)를 읽는다(`hints.rs`). `linguist-*`는 아직 아무도 읽지 않는다.
`.aqr/attributes`에서는 읽지 않는다.

## 4. 구현의 요점

- 힌트가 있는 파일의 Kind는 `Kind::Hint(i)`(aqr-formula)다. `i`는 프로세스에 하나인 표(경로 사슬, 이름의
  Kind, 포맷, 파라미터)의 번호로, `Kind::Format`이 파라미터를 못 싣는 문제를 푼다. 스트림이 Kind를 들고
  가므로 recompressor가 만든 멤버도 힌트를 그대로 들고 간다(`Nest::Hint(i)`, `hints::made`). 인코딩 전에
  `hints::settle`이 Kind를 힌트가 없었을 때의 것으로 되돌린다.
- 컨테이너의 멤버: codec crate의 `kind`는 `aqr_formula::Kinds`(클로저)다. recompressor를 부를 때 데이터가 힌트를
  가진 파일이면(`Cx::within`) 스레드 국소로 두고 `hints::kinds()`가 멤버마다 사슬을 한 층 늘인 힌트를 준다.
  이름 없는 내용 하나뿐인 포맷은 빈 경로를 준다. `aqr-not`과 `binary`는 감지의 스레드 국소 마스크다.
- 보고(`Hints::report`): 알아듣지 못한 속성, 포맷이 거절한 파일, 맞는 파일이 없던 줄. `-v`, `--stats`의
  "what stands out", JSON의 `hints`.
- 큰 헤더 없는 텐서 파일(1 GiB부터)도 혼자 파일일 때 직접 쓴다(`direct_room`이 힌트의 레이아웃으로
  공식의 상한을 잰다, `formula_bound_layout`). `aqr-offset`이 있으면 직접 쓰지 않는다.

## 5. 예: loupe의 가중치

`D:\Works\git\loupe`(브라우저 확장의 OCR)는 두 모델의 가중치를 헤더 없는 F16 배열로 둔다
(`det_weights_f16.bin`, `rec_weights_f16.bin`; 텐서의 이름·모양·오프셋은 `model_manifest.json`, 빈틈 없이
det 257개, rec 161개). 같은 두 파일이 `loupe.zip` 안에도 deflate로 있다. `cargo aqr` 기본 레벨, 2026-11-24
(맨 아래 줄은 aqr-tensors에 0·비정규 수의 기호와 행의 지수 클래스가 든 뒤):

| | det (30,976,706) | rec (38,230,316) |
|---|---|---|
| 힌트 없이(zstd) | 14,797,379 | 32,401,955 |
| `aqr-dtype=f16` | 14,053,139 (−5.0%) | 31,941,115 (−1.4%) |
| `aqr-layout` | 12,476,817 (−15.7%) | 29,599,423 (−8.6%) |
| xz −6(참고) | 13,281,216 | 30,959,376 |

```
# loupe의 .aqr/attributes
[attr]f16-tensors aqr-format=tensors aqr-dtype=f16
*_f16.bin            f16-tensors
loupe.zip>*_f16.bin  f16-tensors
```

(`aqr-layout`은 `--attributes`로 트리 밖에서 준 레이아웃 TOML을 가리킨다.) 트리 전체(17개 파일, 117.21 MB)는
48,526,012 → 43,403,172(−10.6%). 알게 된 것:

- 경계가 중요하다. 텐서마다 표가 따로 서므로 `aqr-dtype`만으로는 부족하고 배치(`aqr-layout`)를 주는 편이 두
  배 이상 얻는다.
- 힌트가 길을 열어도 끝의 recompressor가 그 데이터를 다룰 수 있어야 한다. det의 큰 텐서는 97–99%가 0인데
  처음 aqr-tensors는 0에도 지수·부호·가수 11비트를 냈고 xz −6보다 컸다. 지금은 0과 비정규 수가 따로 기호이고
  행은 지수에 따라 최대 8클래스로 나뉘어 저마다 표를 쓴다(aqr-tensors FORMAT.md §4.2). 이 뒤 둘 다 xz −6보다
  작다.

## 6. 이후 [제안]

- `aqr --suggest-attributes DIR`: 비싼 분석(오프셋 스캔, 헤더 없는 PCM·텐서의 dtype 추정, aqr-infer-pak의
  추론)을 한 번 돌려 `.aqr/attributes`와 §3.4의 데이터 파일을 쓴다. 힌트가 분석 결과의 캐시가 된다.
- aqr-infer-pak: 비용과 오탐으로 aqr-compress에서 빠졌다(`docs/file-formats.md`). `aqr-format=pak`(추론)과
  `aqr-template=FILE`(검증만)으로 opt-in 복원. 그 crate의 `template`이 이미 그 모양이다.
- Kaitai Struct(`.ksy`): `aqr-struct=FILE`. `seq`, `instances`, `size`, `repeat`, `process` 정도만 읽는다.
  `.ksy`는 파일마다 라이선스가 달라 번들하지 않고 사용자가 가리킨 것만 읽는다.
- `magic(5)`의 부분집합(바이트로 포맷을 고르는 사용자 규칙). 쓰임이 좁아 뒤로 미룬다.
- 보고의 구분: 지금은 거절만 나오고, "공식이 데이터보다 작지 않음"(스캔의 `smaller`, `Cx::lost`)과 "비슷해서
  배치가 고르도록 곁에 둠"(그것이 결국 골라졌는지도)은 보고하지 않는다.

## 7. 정하지 않은 것

- 디렉터리마다 `.aqr/attributes`를 둘지(§2.1). 지금은 루트 하나.
- 사용자 전역 파일(`~/.config/aqr/attributes`, git의 `core.attributesFile`). 재현성을 해치므로 넣더라도
  `--stats`에 읽었다고 적는다.
- 컨테이너 안의 `.aqr/attributes`는 읽지 않는다(바깥 트리의 파일이 `>`로 말한다).

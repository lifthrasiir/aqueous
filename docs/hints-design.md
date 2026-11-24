# 파일에 주는 힌트 (`.aqr/attributes`)

aqr-compress는 파일을 앞부분의 바이트(`detect`, `crates/aqr-compress/src/detect.rs`)와 이름의
확장자(`kind_by_name`)로 분류한다. 헤더 없는 데이터, 다른 데이터 안에 들어 있는 스트림, 확장자가
흔한 이름(`.bin`, `.dat`)인 파일은 둘 다 놓친다. 그런 파일은 아무 처리 없이 zstd로 저장된다. 이 문서는
사용자가 경로 패턴별로 포맷과 파라미터를 알려 주는 방법을 정한다.

## 1. 원칙

- **힌트는 인코더에만 쓰인다.** 복원은 포뮬러가 전부 기술하므로, 힌트는 아카이브의 형식(spec)에 아무것도
  더하지 않는다. 잘못된 힌트는 크기와 시간만 잃고 정확성은 잃지 않는다.
- **힌트는 후보이지 단정이 아니다.** 힌트가 가리킨 recompressor가 데이터를 거절하면, 그 파일은 힌트가
  없었을 때와 같이 처리된다. 다만 `aqr-format`은 지금의 `Kind::Format`과 같이 감지를 건너뛰고 먼저
  시도하는 것이며, 거절되면 감지로 돌아간다(§3.1).
- **힌트는 압축한 트리와 함께 다닌다.** `.aqr/attributes`는 보통 파일로 아카이브에 들어간다. 풀었다가 다시
  압축해도 같은 결과가 나온다. `.aqr/sfx/`(`docs/sfx-config-design.md`)와 같은 자리이지만, 아카이브에서
  읽어 되돌리는 일은 없다. 힌트는 압축할 때만 읽는다.
- **opt-in을 우회하는 통로가 되지 않는다.** 내용을 보존하는 손실 정규화(`-Y`, `docs/lossy-design.md`)는
  속성으로 켜지 않는다. 아카이브 안에 든 파일 하나가 압축하는 사람의 선택을 바꾸면 안 된다.

## 2. 파일과 문법

### 2.1 어디서 읽는가

1. `-Z` 옵션(지금 있는 `-Z format:no-NAME` 등)
2. `--attributes FILE`(여러 번): 트리를 고칠 수 없을 때. 패턴은 입력의 루트를 기준으로 한다
3. 입력 트리의 `.aqr/attributes`: 각 입력의 루트에 있는 것만 읽는다
4. 입력 트리의 `.gitattributes`: 디렉터리마다 있는 것을 읽는다(git과 같이)
5. 감지(`detect`)와 이름(`kind_by_name`)

먼저 나온 것이 이긴다. 같은 단계 안에서는 git의 규칙을 따른다. 뒤에 나온 줄이 이기고, 더 깊은 디렉터리의
`.gitattributes`가 이긴다. `.aqr/attributes`를 루트 하나로 한정하는 것은 트리 전체의 힌트를 한 파일에서
보게 하려는 것이다. 디렉터리마다 두는 것은 쓰임이 보일 때 더한다.

입력이 트리가 아니라 파일 하나이면(`aqr -c foo.bin`) 3과 4는 없다. 그 파일의 부모 디렉터리를 거슬러
올라가며 찾지 않는다. 입력 밖의 파일이 결과를 바꾸면 압축이 재현되지 않는다. 파일 하나에는 `--attributes`를
쓴다.

`-a`(덧붙이기)는 덧붙이는 입력의 `.aqr/attributes`를 읽는다. 아카이브에 이미 든 것은 읽지 않는다.

### 2.2 gitattributes 문법

`.aqr/attributes`는 gitattributes(5)의 문법과 의미를 그대로 따른다. 다른 문법을 비슷하게 만드는 것보다
그대로 따르는 편이 낫다.

- 한 줄에 패턴 하나와 속성 여럿: `PATTERN attr -attr !attr attr=value`
- 패턴은 gitignore의 것이다. `/`가 없으면 어느 깊이의 기본 이름이든 맞고, 있으면 파일이 있는 디렉터리를
  기준으로 한다. `**`가 있다. 부정 패턴(`!`)은 없다. 디렉터리 패턴은 그 안의 파일에 걸리지 않으므로
  `dir/**`로 쓴다
- `[attr]NAME …`: macro 속성. git의 것과 같이 `.aqr/attributes`에서만 정의할 수 있다(git은 루트의
  `.gitattributes`에서도 허용하므로, `.gitattributes`의 macro도 읽는다)
- 패턴은 C 식으로 따옴표를 쳐도 된다(`"a b.bin"`). 값에는 공백을 넣을 수 없다. 값이 길어지면 파일을
  가리킨다(§3.4)

파서는 직접 쓴다. gix-attributes는 MIT/Apache이지만 gix의 다른 crate들을 끌고 온다. 문법이 작아서
직접 쓰는 편이 싸다.

### 2.3 컨테이너 안쪽 경로: `>`

`.aqr/attributes`의 패턴에서 `>`는 컨테이너 안으로 들어가는 구분자다.

```
loupe.zip>*.bin            aqr-format=tensors aqr-dtype=f16
game/*.pak>sounds/**       aqr-kind=compressed
backup.tar.zst>>db/*.dat   aqr-format=sqlite
```

- `>` 앞은 지금 패턴이 있는 트리에서의 경로, 뒤는 컨테이너의 루트에서의 경로이며, 각 부분이 §2.2의
  패턴 규칙을 따로 따른다(`loupe.zip>*.bin`의 `*.bin`은 zip 안의 어느 깊이에서든 맞는다)
- 컨테이너란 aqr-compress가 이름 붙은 멤버로 나누는 모든 것이다: ZIP, tar, 7z, ALZ, EGG, asar, 디스크
  이미지의 파일 시스템, aqr-bundle의 packs
- 이름 없는 내용 하나만 있는 것(gzip, xz, zstd처럼 스트림을 풀면 내용 하나가 나오는 것)은 `>` 뒤를 비운다.
  `backup.tar.zst>`는 풀린 tar 자체이고, `backup.tar.zst>>db/*.dat`은 그 tar 안의 파일이다
- `>`로 들어가지 않는 패턴은 컨테이너 안의 멤버에 맞지 않는다. `*.bin`은 트리의 파일에만 걸리고
  `loupe.zip` 안의 `.bin`에는 걸리지 않는다. 이것이 git의 의미에 맞고, 트리 수준의 힌트가 예상 밖의
  깊이에서 적용되는 일을 막는다. 모든 깊이에 걸려면 `**>*.bin`이 아니라 두 줄을 쓴다(`>`의 개수가
  깊이를 정한다)
- Windows에서는 `>`가 파일 이름에 올 수 없다. Unix에서 이름에 `>`가 든 파일은 `\>`로 쓴다
- `.gitattributes`에서는 `>`를 구분자로 읽지 않는다. git은 `>`를 보통 글자로 읽으므로 같은 패턴이 두 도구에서
  다른 뜻이 된다. `.gitattributes`의 속성은 트리의 파일에만 걸린다

## 3. 속성

모두 `aqr-` 접두사를 쓴다. git이 모르는 속성은 무시하므로, 저장소의 `.gitattributes`에 적어도 git에는 아무
영향이 없다.

### 3.1 포맷: `aqr-format=NAME`

`NAME`은 `Format::from_name`의 이름(`zlib`, `png`, `safetensors`, …) 또는 §3.3의 파라미터를 받는 이름이다.
파일의 Kind를 `Kind::Format(NAME)`으로 만든다. 이것이 이미 하는 일은 "첫 바이트부터 그 포맷으로 보고
감지하지 않는다"이다. 그 recompressor가 거절하면 감지(`detect`)로 돌아간다. `Kind::Format`의 지금 뜻
("거절되면 그대로 둔다")과 다르므로, 힌트에서 온 것은 따로 표시한다.

`-aqr-format`은 recompressor를 찾지 않는다(감지도 하지 않는다). Kind는 `aqr-kind`나 이름이 정한다.

### 3.2 종류와 금지

- `aqr-kind=KIND`: `Kind`의 `Display` 이름(`unknown`, `pixels`, `text`, `font`, `table`, `compressed`).
  `compressed`는 시도를 건너뛰어 시간을 아낀다. 암호화된 데이터, 이미 압축된 독자 포맷에 쓴다
- `aqr-not=NAME,NAME`: 그 파일들에서 그 포맷들을 감지하지 않는다. `-Z format:no-NAME`의 경로별
  버전이다. 감지가 잘못 맞아 시간만 쓰는 경우에 쓴다

### 3.3 파라미터

헤더가 없어서 recompressor가 스스로 찾을 수 없는 것을 준다. 파라미터는 포맷마다 정의하고, 그 포맷의
README에 적는다. 이 문서는 모양만 정한다.

- 이름은 `aqr-PARAM`(`aqr-dtype=f16`, `aqr-bits=16`, `aqr-channels=2`, `aqr-offset=0x200`)
- `aqr-format`이 맞는 파일에서만 읽는다. 다른 포맷의 파라미터는 무시한다
- 모르는 값이면 경고하고 그 힌트를 버린다(감지로 돌아간다)

처음 정의할 것은 §5의 예에 필요한 것들이다.

| 포맷 | 파라미터 | 뜻 |
|---|---|---|
| `tensors` | `aqr-dtype=bf16\|f16\|f32\|…` | 헤더 없는 텐서 한 개로 본다. 원소 수는 길이/원소 크기 |
| `tensors` | `aqr-layout=FILE` | 텐서들의 경계와 dtype(§3.4) |
| `raw` | `aqr-bits`, `aqr-channels`, `aqr-endian` | 헤더 없는 PCM 표본 |
| `zlib`, `deflate` | `aqr-offset` | 그 오프셋에서 시작하는 스트림 |

### 3.4 무거운 데이터는 파일로

한 줄에 들어가지 않는 것(텐서의 배치, aqr-infer-pak의 template, 오프셋 목록)은 `.aqr/` 아래의 파일로 두고
속성은 그 파일을 가리킨다. 예: `aqr-layout=.aqr/layouts/det.toml`. 경로는 `.aqr/attributes`가 있는 루트를
기준으로 한다. 그 파일들도 아카이브에 들어간다.

이렇게 나누면 선택(어느 파일에)과 데이터(무엇을)가 분리된다. 데이터 파일은 사람이 쓰기보다 도구가 쓰는
경우가 많을 것이다(§6).

### 3.5 git의 표준 속성

`.gitattributes`에서는 `aqr-*`와 함께 다음을 약한 힌트로 쓴다. 감지보다 약하다. 즉 바이트가 포맷을
말하면 그것이 이긴다.

| 속성 | 쓰임 |
|---|---|
| `binary`, `-text` | 텍스트가 아니다. aqr-log, aqr-xsv, aqr-wrt의 표본 추출을 건너뛴다 |
| `text`, `eol=…` | 텍스트다 |
| `filter=lfs` | LFS 포인터, 작은 텍스트 |
| `linguist-language=…`, `linguist-generated`, `linguist-vendored` | 코드인지 산문인지. aqr-wrt가 쓸 수 있다 |

`.aqr/attributes`에서는 이 속성들을 읽지 않는다. aqr의 파일에는 aqr의 이름만 쓴다.

## 4. 구현 지점

- 트리의 파일: `crates/aqr-compress/src/archive.rs`의 `File::of(…, kind_by_name(src.name(c)), …)`
  두 곳(896, 1150행 무렵)이 이름에서 Kind를 얻는다. 여기서 속성을 먼저 본다. 파일 하나는
  `crates/aqr/src/compress.rs`의 `kind_by_name(&name)`
- 컨테이너의 멤버: recompressor들은 Kind를 `kind: fn(&[u8]) -> Kind`로 받는다(aqr-alz, aqr-egg의
  `Options`, aqr-bundle packs의 `encode_with`). 이것을 컨테이너의 경로를 품은 `&dyn Fn(&[u8]) -> Hint`로
  바꾼다. `>`의 깊이는 이 클로저가 한 층씩 들고 내려간다
- `Kind::Format(&'static str)`은 파라미터를 싣지 못한다. 힌트는 `Kind`와 별도의 `Hint`(포맷, 파라미터,
  힌트에서 왔다는 표시)로 `Cx`를 따라 넘기고, `Kind`에는 지금과 같이 이름만 남긴다
- 보고: 힌트가 맞았는데 recompressor가 거절했거나 실패한 파일, 맞는 파일이 없었던 패턴을 `--stats`의
  "what stands out"에 적는다. 이것이 없으면 사용자가 힌트를 디버그할 수 없다
- 시험: 패턴(특히 `>`와 git의 디렉터리 규칙), 우선순위, 거절 후 감지로 돌아가는 경로, 그리고 힌트 없이
  만든 아카이브와 힌트를 준 아카이브가 같은 내용으로 풀리는지

## 5. 예: loupe의 가중치

`D:\Works\git\loupe`(브라우저 확장의 OCR)는 두 모델의 가중치를 헤더 없는 F16 배열로 둔다
(`det_weights_f16.bin`, `rec_weights_f16.bin`). 텐서의 이름, 모양, 오프셋은 `model_manifest.json`에 있다.
오프셋은 F32로 바꾼 뒤의 바이트이므로 파일에서는 그 절반이다. 텐서들은 빈틈 없이 파일을 채운다
(det 257개, rec 161개). 같은 두 파일이 `loupe.zip` 안에도 deflate로 들어 있다.

지금 aqr는 이것들을 zstd로 저장한다. 힌트가 aqr-tensors로 보낼 때 얻는 것을 재려고, 같은 데이터를
safetensors로 감쌌다. 텐서 하나로 감싼 것은 `aqr-dtype=f16`에, manifest의 텐서들로 감싼 것은
`aqr-layout`에 해당한다. 크기에는 safetensors 헤더가 들어 있다(텐서 하나는 80 B, 여럿은 det 23 KB와
rec 14 KB를 그대로 둔 것이다). `cargo aqr` 기본 레벨, 2026-11-24.

| | det (30,976,706) | rec (38,230,316) |
|---|---|---|
| 지금(zstd) | 14,797,379 | 32,401,955 |
| `aqr-dtype=f16`(텐서 하나) | 14,390,825 (−2.7%) | 32,240,307 (−0.5%) |
| `aqr-layout`(manifest의 텐서들) | 14,006,125 (−5.3%) | 31,748,053 (−2.0%) |
| xz −6(참고) | 13,281,216 | 30,959,376 |

```
# loupe의 .aqr/attributes
[attr]f16-tensors aqr-format=tensors aqr-dtype=f16
*_f16.bin            f16-tensors
loupe.zip>*_f16.bin  f16-tensors
```

알게 된 것:

- 경계가 중요하다. 텐서마다 표가 따로 서므로, 하나로 보는 것보다 배치를 주는 편이 두 배 이상 얻는다. 그러니
  `aqr-dtype`만으로는 부족하고 `aqr-layout`이 있어야 한다. 배치 파일은 manifest에서 스크립트로 만든다.
  manifest 같은 앱 고유의 파일을 aqr가 직접 읽게 하지는 않는다
- 힌트를 준 aqr-tensors도 xz −6보다 크다. det의 큰 텐서들은 절반이 0이고(가지치기), 16비트 값의 0차
  엔트로피가 1.1–1.4비트다. aqr-tensors는 지수를 0차로 코딩하고 가수는 그대로 두므로 0이 많은 텐서를
  살리지 못한다. rec(16비트 값의 엔트로피 13–14비트)에서도 xz가 조금 낫다. 힌트 설계의 문제가 아니라
  aqr-tensors의 남은 일이며, 그 README의 Status에 올린다(0이 많은 텐서, 가수와 지수의 관계)
- 이 측정은 힌트가 길을 열어 주어도 그 끝의 recompressor가 그 데이터를 다룰 수 있어야 한다는 것을
  보여 준다. `--stats`의 보고(§4)에 "힌트를 받은 recompressor가 zstd보다 못했다"도 넣는다

## 6. 이후

- `aqr --suggest-attributes DIR`: 비싼 분석을 한 번 돌려 `.aqr/attributes`와 §3.4의 데이터 파일을 쓴다.
  오프셋 스캔, 헤더 없는 PCM이나 텐서의 dtype 추정, aqr-infer-pak의 추론이 그것이다. 힌트는 사람이 쓰는
  설정이면서 그 분석 결과의 캐시가 된다
- aqr-infer-pak: 비용과 오탐 때문에 aqr-compress에서 빠졌다(`docs/file-formats.md`). `aqr-format=pak`
  (추론)과 `aqr-template=FILE`(검증만)으로 opt-in으로 되살린다. 그 crate의 `template`이 이미 그 모양이다
- Kaitai Struct(`.ksy`): `aqr-struct=FILE`. `process: zlib` 같은 필드가 압축 스트림의 위치를 정확히
  말한다. `seq`, `instances`, `size`, `repeat`, `process` 정도만 읽어도 된다. `.ksy`는 파일마다 라이선스가
  다르므로 번들하지 않고, 사용자가 가리키는 것만 읽는다
- `magic(5)`의 부분집합: 경로가 아니라 바이트로 포맷을 고르는 사용자 규칙(`detect.rs`의 `Test::Magic`과
  같은 뜻). 쓰임이 좁아서 뒤로 미룬다

## 7. 정하지 않은 것

- 디렉터리마다 `.aqr/attributes`를 둘지(§2.1). 지금은 루트 하나
- 사용자 전역 파일(git의 `core.attributesFile`에 해당하는 `~/.config/aqr/attributes`). 압축의 재현성을
  해치므로 넣더라도 `--stats`에 읽었다고 적는다
- 컨테이너 안의 `.aqr/attributes`(zip 안에 든 것)를 읽을지. 지금은 읽지 않는다. 바깥 트리의 파일이
  `>`로 말한다

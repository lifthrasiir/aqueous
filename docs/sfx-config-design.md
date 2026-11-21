# 자기 압축 해제 프로그램의 설정 (`.aqr/sfx/`)

지금 `aqr --format sfx`가 만드는 자기 압축 해제 프로그램(이하 SFX)은 정해진 일 하나만 한다. 명령줄의
`DIR`(기본은 현재 폴더) 아래에 파일을 풀어 놓는다 (`crates/aqr-sfx-shim/src/main.rs`). 이 문서는
압축이 끝난 뒤 프로그램을 실행하는 일, 아이콘, 라이선스 동의처럼 다른 SFX 도구들이 갖춘 설정을 어떻게
받고 어디에 싣는지 정한다. 서브시스템(GUI/CLI)과 암호는 `encrypted-sfx-design.md`가 다루며, 여기서는
그 결정을 전제로 한다.

## 1. 다른 도구들이 두는 설정

참고한 도구: 7-Zip SFX(`config.txt`, 그 확장인 7zSD), WinRAR SFX(주석 스크립트), IExpress(SED),
WinZip Self-Extractor, makeself.

### 1.1 압축 해제 뒤에 하는 일

| 설정 | 예 | 비고 |
|---|---|---|
| 압축 해제 뒤 실행 | 7z `RunProgram`, RAR `Setup=`, IExpress `AppLaunched` | 핵심 설정이다. 인자, 작업 디렉터리, 창 상태도 함께 받는다 |
| 압축 해제 전 실행 | RAR `Presetup=` | 쓰이는 일이 드물다 |
| 뒤이은 명령 | IExpress `PostInstallCmd` | 실행이 끝난 뒤 하나 더 |
| 종료를 기다린 뒤 정리 | RAR `TempMode`, 7z 기본 | 임시 폴더에 풀고, 실행하고, 기다리고, 지운다. 설치 프로그램형 SFX의 기본 모드다 |
| 종료 코드 전달 | | 실행한 프로그램의 종료 코드를 SFX의 종료 코드로 돌려준다. 자동화에 필요하다 |
| 재부팅 | IExpress `RebootMode` | |
| 바로가기 | RAR `Shortcut=`, 7zSD | 바탕 화면, 시작 메뉴 |
| 환경 변수 | 7zSD `SetEnvironment` | 실행하는 프로그램에 넘긴다 |

### 1.2 대상 위치와 파일

| 설정 | 비고 |
|---|---|
| 기본 대상 경로 | 임시 폴더, Program Files, SFX가 있는 폴더, 현재 폴더 |
| 경로를 바꿀 수 있는지 | 묻는 대화상자를 띄울지, 고정할지 |
| 하위 폴더 만들기 | 아카이브 이름으로 폴더를 만든다 (`foo.exe` → `foo\`). 파일이 대상 폴더에 흩어지는 것을 막는다 |
| 덮어쓰기 | 묻기, 항상, 안 함, 더 새로운 것만 (RAR `Overwrite`, `Update`) |
| 기존 파일 지우기 | RAR `Delete=` |

### 1.3 UI와 문구

창 제목, 시작 전 확인(7z `BeginPrompt`), 라이선스 동의(RAR `License=`, IExpress `DisplayLicense`:
거부하면 끝낸다), 완료 메시지(IExpress `FinishMessage`), 무인 모드(RAR `Silent`, 7z `GUIMode`,
진행 창을 보일지 포함), 로고나 배너 이미지(RAR `-iimg`), 언어별 문구.

### 1.4 PE 자체의 속성

| 설정 | 비고 |
|---|---|
| 아이콘 | 탐색기는 리소스만 읽는다. 런타임에는 바꿀 수 없다 |
| `VERSIONINFO` | 제품명, 회사, 버전. 파일 속성 창과 SmartScreen 평판에 쓰인다 |
| manifest | `requireAdministrator`(RAR `-iadm`), DPI, Common Controls v6, `consoleAllocationPolicy` |
| 서브시스템 | `encrypted-sfx-design.md` 2절 |
| 코드 서명 | aqr 밖(signtool)의 일이다. 다만 서명한 뒤에 무엇을 덧붙이면 서명이 깨지므로, SFX는 다 만든 뒤에 서명해야 한다 |

### 1.5 명령줄

대상 경로(지금의 `DIR`), 묻지 않기(`-y`), 암호(`-P`), 실행하지 않고 풀기만 하기(makeself
`--noexec`), 임시 폴더를 지우지 않기(`--keep`). 이것들은 `.aqr/sfx/`가 아니라 SFX의 명령줄로 받는다.

## 2. 결정: 원본은 아카이브에, 컴파일된 설정은 실행 파일에

설정은 aqr의 옵션으로 받지 않는다. 압축하는 입력의 최상위에 `.aqr/sfx/`가 있으면 그 안의 파일로
정한다.

- **원본**: `.aqr/sfx/` 아래의 파일은 `sfx.toml`을 포함해 모두 그대로 아카이브에 들어간다. 그래서
  `aqr -d`로 풀고 다시 `--format sfx`로 만들면 같은 SFX가 나온다. 설정을 실행 파일에서 거꾸로
  되살리는 길은 필요 없다.
- **컴파일된 설정**: 압축하는 시점에 aqr이 `sfx.toml`을 파싱해서 둘로 나눠 싣는다.
  - PE의 속성: 아이콘, `VERSIONINFO`, manifest, 서브시스템 (1.4절).
  - 설정 blob: 나머지 전부 (1.1–1.3절). CBOR로 쓴다 (4절).
- **SFX가 만들어진 뒤에는 아카이브 부분을 고쳐도 SFX의 동작이 바뀌지 않는다.** stub은 런타임에
  `sfx.toml`을 읽지 않는다. 그래서 stub에 TOML 파서가 들어가지 않고, 지금처럼 작게 유지된다
  (0.78 MB, UPX로 0.35 MB). 큰 파일을 어떻게 이 원칙 안에 두는지는 2.1절에서 다룬다. 애초에 aqr이
  SFX 속의 아카이브를 고치지 못하게 하는 규칙도 둔다 (7절).

### 2.1 큰 설정은 파일로, 작은 설정은 toml로

긴 텍스트와 바이너리는 `.aqr/sfx/` 아래의 별도 파일로 두고, `sfx.toml`은 그것을 경로로 가리킨다.
스칼라 값, 목록, 짧은 문구만 toml에 남긴다.

| 파일 | 실리는 곳 |
|---|---|
| `sfx.toml` | 파싱된 결과가 PE 속성과 설정 blob으로 |
| `icon.ico` (toml이 가리키는 것) | PE 리소스 (탐색기가 읽으므로 PE에 복사할 수밖에 없다) |
| `license.txt`, `license.ko.txt`, … | 아카이브의 첫 세그먼트에만 |
| `banner.png` | 아카이브의 첫 세그먼트에만 |
| `manifest.xml` (있으면 기본 manifest 대신) | PE 리소스 |

라이선스와 배너는 압축 해제보다 먼저 보여야 한다. 그렇다고 설정 blob에 복사하면 아카이브 밖에 있게
되어 압축의 효과를 덜 본다. 그 대신 **`.aqr/sfx/`를 아카이브의 첫 세그먼트에 두고, 나머지 파일은 그
뒤의 세그먼트에 둔다.** stub은 `.aqr/sfx/`의 파일을 첫 세그먼트에서만 찾는다.

- 첫 세그먼트는 그 자체로 하나의 아카이브로 열린다. `aqr::open_upto`가 첫 저널 지점(첫 세그먼트의
  메타데이터 블록)에서 연다 (스펙, "`journal`"). 그 트리에는 `.aqr/sfx/`만 있다. 그래서 stub은
  `contents_of`로 그 파일들만 디코딩하고, 나머지 세그먼트의 블록은 건드리지 않는다.
- 설정 blob은 파일을 경로(`.aqr/sfx/license.txt`)로 가리킨다. 블록 번호는 압축기가 블록을 어떻게
  나누는지에 묶이므로 쓰지 않는다. 경로를 쓰더라도 첫 세그먼트에서만 찾으므로, 나중에 덧붙인
  세그먼트가 같은 경로를 덮어써도 SFX가 보여 주는 것은 바뀌지 않는다.
- 세그먼트를 나누는 비용은 메타데이터 블록 하나와 그 세그먼트의 모듈 블록이다. `.aqr/sfx/`가 있을
  때만 든다. 첫 세그먼트의 모듈(텍스트, PNG)이 둘째 세그먼트에서 다시 쓰이면, 모듈 블록이 중복되지
  않는지 확인해야 한다.

### 2.2 `sfx.toml`의 모양 (초안)

```toml
mode = "run"                  # "extract" | "run": 아래 기본값의 묶음
title = "Example Setup"

[extract]
dest = "{temp}"               # {temp} {here} {cwd} {programs} 중 하나로 시작
ask = false                   # 경로를 묻는지
subdir = true                 # 아카이브 이름으로 폴더를 만드는지
overwrite = "ask"             # "ask" | "always" | "never" | "newer"

[run]
program = ["setup.exe", "/S"] # argv. 첫 원소는 아카이브 안의 상대 경로
cwd = "."                     # 압축을 푼 폴더 기준
window = "normal"             # "normal" | "hidden" | "minimized"
wait = true
cleanup = true                # wait가 참일 때, 끝난 뒤 지우는지
exit-code = true              # 프로그램의 종료 코드를 돌려주는지

[ui]
license = "license.txt"       # .aqr/sfx/ 기준. license.<언어>.txt가 있으면 그쪽을 고른다
banner = "banner.png"         # .aqr/sfx/ 기준, 언어별 파일은 license와 같다. PNG, BMP, JPEG, GIF
prompt = "Install Example?"
finish = "Done."
quiet = false                 # 무인 모드. 명령줄 -y와 같은 효과

[pe]
icon = "icon.ico"
admin = false                 # manifest의 requireAdministrator
subsystem = "gui"             # "gui" | "cli"
version = "1.2.3.0"
product = "Example"
company = "Example Inc."
description = "Example installer"
```

- `mode`는 기본값의 묶음이다. `extract`는 경로를 묻고 풀어 둔 뒤 끝낸다. `run`은 임시 폴더에 풀고,
  실행하고, 기다리고, 지우고, 종료 코드를 돌려준다. 나머지 키는 그 위에 덮어쓴다. 이렇게 하면 조합의
  수가 줄어든다.
- `program`은 셸 문자열이 아니라 argv 배열이다. 따옴표 처리가 갈리지 않고, 다른 플랫폼의 stub이
  생겨도 그대로 쓸 수 있다. 실행은 `CreateProcessW`로 하고 셸(cmd)을 거치지 않는다. 첫 원소는
  아카이브 안의 `.exe`(또는 `.com`)여야 한다(3절, 8절). 그래서 `.bat`, `.cmd`, `.msi`는 지금 실행할 수
  없다. 시스템의 `cmd`나 `msiexec`를 부르는 것은 8절의 "임의 명령"이 되기 때문이다. 필요해지면 그런
  것만 따로 허용하는 키를 검토한다.
- 경로 변수(`{temp}` 등)는 몇 개로 고정하고, 런타임에 stub이 해석한다. Windows 환경 변수
  (`%VAR%`)를 그대로 확장하게 하면 stub이 그 문법에 묶인다.

## 3. 압축 시점의 검증

컴파일하는 단계에서 다음을 확인하고 실패시킨다. SFX를 실행했을 때가 아니라 만들 때 오류가 드러난다.

- `run.program[0]`이 아카이브 안에 실제로 있는지. 상대 경로이고 `..`가 없는지.
- `ui.license`, `ui.banner`, `pe.icon`, `manifest.xml`이 `.aqr/sfx/` 안에 있는지. 아이콘이 올바른
  `.ico`인지.
- 모르는 키가 있는지. 오타가 조용히 무시되지 않게 한다.
- `--format sfx`가 아닌데 `.aqr/sfx/`가 있으면 경고만 하고, 일반 파일로 넣는다.

## 4. 설정 blob

- 형식은 CBOR 데이터 항목 하나다 (RFC 8949). 아카이브의 선택적 메타데이터가 이미 CBOR이고
  (스펙, "Optional metadata"), 그것을 읽는 코드(aqr-formula의 `aqr/tree.rs`: 어떤 CBOR이든 읽고 아는
  것만 취한다)는 트리를 푸는 stub에 이미 링크되어 있다. 확장할 때도 키를 하나 더하면 된다.
- 키의 규칙은 스펙의 선택적 메타데이터를 따른다. 바깥은 음수 정수 키의 맵이고, 키마다 이름을 붙여
  CDDL로 적는다 (`&(run: -1) => …`처럼). 모르는 키는 무시한다. 하위 호환이 깨지는 변경은 키를
  더하는 대신 버전 키를 올리고, stub은 모르는 버전이면 실패한다. 지금은 aqr에 stub이 들어 있어 둘이
  항상 함께 배포되므로, 버전이 어긋나는 일은 aqr-ld merge로 만든 SFX처럼 stub을 따로 다루는 경우뿐이다.
- 문자열은 CBOR의 text string(UTF-8)이다. stub이 Win32 API에 넘길 때 UTF-16으로 바꾼다.
- 번들의 엔트리로 두지 않고, 번들과 아카이브 사이에 레코드 하나로 둔다:
  `"AQRSFXC1" le64:n cbor[n] le64:n "AQRSFXC1"`. 번들의 엔트리는 모두 blob이라
  (`Blob::from_bytes`, PE의 사실들) 설정을 끼우려면 번들의 형식을 바꿔야 했고, 번들이 없는 SFX에
  설정만 담은 번들을 붙이는 일도 생긴다. 레코드는 앞뒤에 매직과 길이가 있어서, stub은 번들 뒤에서
  앞으로 읽거나, 뒤에서 훑어 찾은 아카이브(aqr-ld merge로 만든 stub, ELF 등 PE가 아닌 경우)의 바로
  앞에서 거꾸로 읽는다. PE 리소스에 두는 방법도 있지만 5절의 UPX 문제를 설정까지 떠안게 된다.
- CBOR의 읽기는 aqr-archive의 것을 그대로 쓴다 (`aqr_archive::cbor`: 선택적 메타데이터의 읽기를
  공개한 것). 키의 CDDL은 `crates/aqr-sfx/src/config.rs`에 있다.

## 5. PE 속성을 쓰는 일: UPX

아이콘, `VERSIONINFO`, manifest는 `.rsrc`에 써야 한다. aqr-sfx에는 리소스를 다루는 코드
(`resource.rs`: aqr-sfx-pack `--embed`)가 이미 있다. 문제는 stub이 UPX로 압축되어 있다는 점이다.
UPX는 첫 아이콘, `VERSIONINFO`, manifest를 압축하지 않고 남기지만, 압축된 실행 파일의 `.rsrc`를
나중에 바꿔 끼우는 일은 깨지기 쉽다. 후보는 셋이다.

1. stub을 UPX로 압축할 때 리소스를 압축하지 않게 하고(`--compress-resources=0`), `.rsrc`를 맨 끝
   섹션에 둔다. aqr은 그 섹션만 다시 쓰고 크기와 헤더를 고친다.
2. pack에 UPX 이전의 stub을 싣고, aqr이 리소스를 쓴 뒤 압축한다. 하지만 aqr이 UPX를 갖거나 직접
   구현해야 하므로 비현실적이다.
3. stub에 빈 자리(최대 크기를 정해 둔 아이콘, `VERSIONINFO` 슬롯)를 미리 두고 그 바이트만 패치한다.
   단순하지만 아이콘 크기에 상한이 생긴다.

구현은 이 셋 중 어느 것도 아니다. **리소스를 마지막 섹션 뒤에 새 섹션으로 쓰고, 데이터 디렉터리의
리소스 항목만 그쪽으로 돌린다** (`crates/aqr-sfx/src/pe.rs`). UPX의 섹션(UPX0, UPX1, UPX2: import와
재배치가 UPX2에 있다)은 한 바이트도 건드리지 않으므로 `--compress-resources=0`도 필요 없다. stub에
있던 리소스는 데이터를 옮기지 않고 RVA 그대로 새 디렉터리에 다시 넣는다 (UPX가 실행할 때 풀어 놓는
리소스도 그대로 통한다). 새로 쓰는 종류(아이콘, 그룹 아이콘, `VERSIONINFO`, manifest)의 예전 항목만
빠진다. 지금의 stub에는 리소스가 하나도 없다. 남은 일은 탐색기에서 보이는 아이콘과 속성 창을 눈으로
확인하는 것이다 (aqr의 `tests/sfx.rs`는 리소스를 읽어 보고, UPX로 압축한 stub을 실행한다).

## 6. `.aqr/` 경로

- `.aqr/`는 입력의 최상위에서만 인식한다. 하위 디렉터리의 `.aqr/`는 일반 데이터다. 입력이 여러 개면
  (`aqr -c a b c`) 아카이브의 루트에 오는 것 중 첫 번째 것만 쓰고, 둘 이상 있으면 오류를 낸다.
- `.aqr/` 전체를 예약 경로로 선언한다. 나중에 `.aqr/` 아래에 다른 것(필터, 메타데이터)이 생길 수 있다.
- `.aqr/sfx/`는 압축을 풀 때도 일반 파일처럼 풀린다. 2절대로 이것이 SFX를 다시 만들기 위한 원본이기
  때문이다. SFX의 `run` 모드에서 임시 폴더에 풀 때는 건너뛸 수 있다.

## 7. SFX 속의 아카이브는 읽기 전용

aqr은 SFX 속의 아카이브를 열거나 풀거나 시험(`-d`, `-t`, `-l`)할 수는 있지만, 고치지는(`-a`나
그 밖의 편집) 않는다. 압축기는 그 아카이브가 어떻게 쓰이는지 판단할 수 없다. stub이 무엇을 읽는지,
덧붙인 세그먼트가 stub이 펼친 번들의 모듈 blob 없이도 풀리는지(덧붙인 세그먼트가 새 모듈을 쓰면 stub은
인터프리터 blob이 있어야 한다), 고친 뒤에 서명이 유효한지 같은 것들이다.

- 규칙은 "아카이브가 파일의 맨 앞에서 시작하지 않으면 고치지 않는다"로 둔다. SFX를 알아볼 필요가 없고,
  다른 데이터 뒤에 붙은 아카이브에도 똑같이 들어맞는다. 스펙의 "Appending to an archive"에도 같은
  권고를 더할지 검토한다.
- 이 규칙이 있어도 2.1절의 첫 세그먼트 규칙은 그대로 둔다. 다른 도구로 아카이브 부분을 고치는
  경우에도 SFX의 동작이 바뀌지 않게 하는 것은 이쪽이다.

## 8. 보안과 평판

자동 실행 SFX는 악성 코드가 흔히 쓰는 형태다. stub 하나가 AV 휴리스틱에 걸리면 aqr이 만드는 모든
SFX가 같이 걸린다.

- 실행 대상은 아카이브 안의 상대 경로로만 제한한다. 임의 명령, 압축 해제 전 실행(`Presetup`), 기존
  파일 지우기(`Delete=`)는 두지 않는다.
- `run` 모드의 SFX에서도 명령줄 `--noexec`(풀기만 하기)가 항상 통하게 한다.
- `VERSIONINFO`와 서명이 평판에 크게 영향을 주므로, 문서에 서명 순서(SFX를 다 만든 뒤)를 적는다.
- 서명(2026-11-21): Authenticode 서명은 파일 끝에 인증서 표를 더하고, 그 해시는 덧붙인 번들, 설정,
  아카이브까지 덮는다. stub은 자기 이미지 뒤에서 아카이브를 찾으므로(뒤에 다른 바이트가 와도 된다)
  서명한 SFX도 그대로 돈다. 앞뒤에 모두 다른 바이트가 있는 파일은 스펙의 "Locating an archive"가
  이 경우 하나만 받는다: PE의 인증서 표가 파일 끝을 차지하면 그 앞(0 패딩은 건너뛴다)에서 찾는다
  (5단계, "has a PE certificate table"; `aqr_archive::locate`). 덧붙이면 깨지므로 서명된 stub은 받지 않는다 (pack.py, aqr의 팩 읽기).
  `crates/aqr/tests/sfx.rs`의 `signed_afterwards`가 OpenSSL로 만든 인증서와 `signtool`로 서명하고
  돌려 본다 (둘 중 하나가 없으면 건너뛴다).
  서명은 `aqr -c --format sfx --run-after "signtool sign ... \"%AQR_OUTPUT%\""`처럼 hook으로 이어 붙일 수 있다
  (`crates/aqr/src/hooks.rs`: 아카이브를 쓰는 작업마다, 셸로, 경로는 환경 변수로. 그 시험도 이렇게 서명한다).

## 9. 순서

1. 1차: `mode`, `extract.dest`, `overwrite`, `run.program`, `wait`, `cleanup`, `exit-code`, 무인 모드,
   `pe.icon`, `VERSIONINFO`, `admin`, 컴파일과 검증, 설정 blob, 5절의 리소스 쓰기, 7절의 읽기 전용
2. 2차: 첫 세그먼트 분리(2.1절), 라이선스, 제목, 문구, 언어별 문구(aqr-l10n과 엮을지), 배너, 바로가기,
   환경 변수
3. 나머지(재부팅, 뒤이은 명령 등): 필요해질 때

GUI의 질문(경로 선택, 덮어쓰기 확인, 라이선스)은 `encrypted-sfx-design.md` 4절대로 GUI blob이
맡는다. 그런 질문이 하나라도 있는 설정이면, 암호가 없어도 GUI blob을 붙인다.

## 10. 구현 (2026-11-21)

9절의 1차와 제목, `prompt`, `finish`까지 구현했다.

- `crates/aqr-sfx/src/config.rs`: 설정(`Config`), 그 레코드(4절), `sfx.toml`의 컴파일과 검증
  (`compile`, feature `encode`: aqr만). `crates/aqr-sfx/src/pe.rs`: 아이콘, `VERSIONINFO`, manifest,
  서브시스템 (5절). `crates/aqr/src/sfx.rs`: 만든 아카이브에서 `.aqr/sfx/`의 파일만 디코딩해 컴파일한다
  (`settings`). 원본이 아카이브에 그대로 있다는 것을 이 경로가 보장한다.
- stub(`crates/aqr-sfx-shim`): 명령줄 `[-y] [--noexec] [--keep] [DIR]`, 덮어쓰기 정책
  (`aqr_sfx::extract_with`: 묻기는 y/n/always/never/quit), 실행(`CreateProcessW`, 창 상태), 기다림, 종료
  코드, 정리. 콘솔이 없으면(표준 오류가 없으면) 메시지와 질문은 `MessageBoxW`로 한다. 파일의 수정
  시각도 이제 아카이브대로 둔다 (`newer`에 필요하다).
- 정한 것들:
  - `{temp}`는 임시 폴더 아래에 새로 만든 폴더다. 정리는 stub이 만든 파일과 폴더만 지운다 (원래 있던
    파일은 대상 폴더가 어디든 건드리지 않는다).
  - 기본값: `extract`는 `{cwd}`, `subdir = false`, `overwrite = "ask"`. `run`은 `{temp}`,
    `overwrite = "always"`, `wait = true`, `cleanup`과 `exit-code`는 `wait`를 따른다
    (`wait = false`인데 둘 중 하나를 켜면 오류). `mode`가 없으면 `run.program`이 있을 때 `run`이다.
    `pe.subsystem`의 기본은 `cli`다 (설정이 없는 SFX와 같다).
  - 설정이 없는 SFX는 전과 같다 (현재 폴더, 있는 파일은 실패, stub의 이미지는 그대로).
  - `run` 모드에서는 `.aqr/`를 풀지 않는다 (6절). `--noexec`이면 푼다.
  - manifest: `requestedExecutionLevel`(`admin`), Windows 7–11의 `supportedOS`, PerMonitorV2 DPI,
    Common Controls 6, 그리고 CLI일 때 `consoleAllocationPolicy`. `.aqr/sfx/manifest.xml`이 있으면
    그것을 쓰고, 그때 `admin`을 함께 주면 오류다.
  - `VERSIONINFO`의 문자열: `version`(FileVersion, ProductVersion), `product`, `company`,
    `description`, `copyright`(LegalCopyright).
- 7절: `aqr -a`, `-s`, `--delete`, `--compact`, 저널 되감기는 파일의 맨 앞에서 시작하지 않는 아카이브를
  거부한다 (`crates/aqr/src/append.rs`). 전에는 앞에 다른 바이트가 있는 아카이브도 고쳤고 그것을
  시험했다 (`tests/edit.rs`, `tests/write.rs`: 이제 거부를 시험한다). 스펙의 "Appending to an archive"에
  같은 권고를 더했다. 그래서 섹터를 가로지르는 꼬리말을 지우고 새로 쓰는 append.rs의 경로(앞에 다른
  바이트가 있을 때만 생긴다)는 aqr에서 쓰이지 않는다. 지울지는 따로 정한다.
- 첫 세그먼트(2.1절): aqr은 `.aqr/sfx/`의 파일을 첫 배치에 두고 그 뒤에 세그먼트를 닫는다
  (`aqr_compress::Options::lead`). 첫 세그먼트에는 디렉터리 항목도 함께 들어간다(내용이 없다).
  설정은 첫 세그먼트에서만 읽는다(`aqr_sfx::config::read_files`, stub도 같다). 비용: 데모(0.7 MB)에서
  68바이트, 2.2 MB의 소스에서 132바이트. 모듈 블록이 겹치지 않는다.
- 라이선스(`ui.license`): `.aqr/sfx/` 안의 UTF-8 파일. stub은 사용자 UI 언어(`AQR_SFX_LANG`이 있으면
  그것)로 `license.ko-KR.txt`, `license.ko.txt`, `license.txt` 순으로 첫 세그먼트에서 찾는다. 콘솔에서는
  본문을 보이고 y/n으로 묻는다. 콘솔이 없으면 메모리에서 만든 대화상자(읽기 전용 스크롤 본문,
  Accept/Decline: user32만)를 띄운다. 4절의 GUI blob 대신 stub에 두었다. 대화상자 하나는 템플릿과 창
  프로시저뿐이라 작기 때문이다. 경로 묻기처럼 질문이 더 늘면 그때 blob으로 옮긴다. 거절하면 종료 코드 2.
  `-y`나 `quiet`이면 묻지 않는다.
- 언어별 문구: `title`, `prompt`, `finish`는 문자열이거나 언어 태그별 표다
  (`prompt = { en = "...", ko = "..." }`). 고르는 순서: 태그 전체, 첫 부분 태그, 언어 없음, `en`, 첫 항목.
  stub 자신의 문구(덮어쓰기 질문, 버튼)는 아직 영어뿐이다 (aqr-l10n과 엮는 일은 남았다).
- 환경 변수: `[run.env] NAME = "value"`. 실행하는 프로그램에만 더해진다 (같은 이름은 대소문자를 가리지
  않고 바꾼다). 값과 `run.program`의 인자에서 `{dest}`는 파일을 푼 폴더다 (`setup.exe /D={dest}`).
- 바로가기: `[[shortcut]]`마다 `target`(아카이브의 파일), `name`(문자열이나 언어별 표), `at`
  (`"desktop"` 또는 `"start-menu"`), 시작 메뉴 아래의 `folder`, `args`(`{dest}`), `description`.
  stub이 파일을 쓴 뒤, 프로그램을 실행하기 전에 셸의 `IShellLinkW`로 만든다 (사용자의 바탕 화면과
  시작 메뉴의 프로그램. 모든 사용자의 것은 아직 없다). 파일이 남을 때만 뜻이 있으므로 `run.cleanup`과
  함께 쓰면 오류다. 시험에서는 `AQR_SFX_SHORTCUTS`가 두 폴더를 대신한다.
- stub의 크기: 환경 변수와 바로가기로 733,696 -> 758,784바이트 (UPX로 344,576).
- 배너(`ui.banner`): `.aqr/sfx/`의 이미지(라이선스처럼 언어별 파일을 고른다). 창으로 물을 때만 쓴다.
  `prompt`와 라이선스를 메모리에서 만든 대화상자로 묻고 그 위에 배너를 보인다 (배너가 없으면 `prompt`는
  전처럼 `MessageBoxW`). 콘솔에서는 읽지도 않는다. 디코딩은 Windows의 WIC가 한다
  (`CLSID_WICImagingFactory1`: Windows 7부터. PNG, BMP, JPEG, GIF). 이미지의 크기는 96 DPI 기준이고, 창의
  DPI에 맞춰 WIC의 Fant 보간으로 늘린다. 투명한 부분은 대화상자의 색과 섞는다. 대화상자는 배너보다
  좁으면 배너만큼 넓어지고, 넓으면 배너가 가운데에 온다. 컴파일은 파일이 있는지, 그 magic이 네 형식 중
  하나인지 본다. `prompt`도 라이선스도 없으면 보일 곳이 없으므로 오류다. 디코딩에 실패하면 배너 없이
  묻는다. stub: 758,784 -> 765,952바이트 (UPX로 348,672). 대화상자의 글꼴은 메시지 상자와 같은
  `lfMessageFont`다 (`MS Shell Dlg`의 한글은 9pt에서 굴림의 비트맵이라 뭉개진다).
- 경로 묻기(`extract.ask`): 라이선스 다음에, `dest`(와 `subdir`)로 정해진 경로를 보이고 묻는다. 콘솔에서는
  `Extract to [경로]: `에 한 줄로 답한다 (빈 답은 그 경로, 표준 입력이 끝나면 취소). 창에서는 경로를 고칠
  수 있는 입력란, 찾아보기(셸의 `IFileOpenDialog`, 폴더 선택: Vista부터. 입력란의 경로에서 있는 가장 가까운
  폴더로 연다), 확인과 취소의 대화상자다 (배너가 있으면 그 위에). 취소하면 종료 코드 2. `-y`, `quiet`,
  명령줄의 `DIR`이면 묻지 않는다. `{temp}`가 만든 폴더를 쓰지 않게 되면 지운다. 4절은 GUI의 질문을
  GUI blob에 두었지만, 라이선스처럼 stub의 대화상자로 했다 (user32, 그리고 셸의 COM). stub: 765,952 ->
  771,072바이트 (UPX로 350,720).
- stub 자신의 문구: aqr-l10n의 섹션 `sfx`의 세 문장(라이선스 질문, `Extract to:`, 덮어쓰기 질문)뿐이다.
  언어는 `ui.title` 등과 같이 고른다 (`AQR_SFX_LANG`, 없으면 사용자 UI 언어). 버튼(확인, 취소, 예,
  아니요)은 메시지 상자와 같은 user32의 문자열 표(800번부터: `MB_GetString`의 것)를 `LoadStringW`로
  읽는다. 그래서 운영체제의 언어를 따른다. 라이선스는 Accept/Decline 대신 질문과 예/아니요로 묻고,
  찾아보기 버튼은 `...`이다. aqr-l10n은 섹션마다 따로 된 표를 만들게 바꿨다 (stub에는 `sfx`의 표만
  들어간다). `MB_GetString`을 `GetProcAddress`로 찾았을 때는 Defender가 시험의 SFX를
  `Trojan:Win32/Bearfoos.A!ml`로 잡았다 (바로가기를 만드는 시험만, 매번). stub: 771,072 -> 781,312바이트
  (UPX로 354,304).
- 남은 것: 모든 사용자의 바로가기,
  `consoleAllocationPolicy`를 모르는 Windows에서의 `FreeConsole` 대체, 암호(GUI blob: 암호 묻기를
  stub의 대화상자로 할지는 그때 정한다).

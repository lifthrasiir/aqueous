# 자기 압축 해제 프로그램의 설정 (`.aqr/sfx/`)

`aqr --format sfx`가 만드는 자기 압축 해제 프로그램(이하 SFX)이 압축을 푼 뒤 프로그램을 실행하는 일,
아이콘, 라이선스 동의 같은 설정을 어떻게 받고 어디에 싣는지의 설계다. 구현은 끝났고(`mode`, `extract`,
`run`, `[run.env]`, `[[shortcut]]`, `ui`의 제목과 문구, 라이선스, 배너, 경로 묻기, `pe`의 아이콘, `VERSIONINFO`,
manifest, 서브시스템, 서명), 이 문서는 바꿀 때 필요한 결정과 이유, 남은 일을 적는다. `sfx.toml`의 키와 값은
`crates/aqr-sfx/src/config.rs`(모듈 문서의 CDDL과 `compile`의 검증)가 원본이다. 서브시스템(GUI/CLI)과 암호는
`encrypted-sfx-design.md`가 다룬다. 끝낸 일의 경과는 git 이력에 있다.

## 1. 다른 도구들이 두는 설정 (참고)

참고한 도구: 7-Zip SFX(`config.txt`, 7zSD), WinRAR SFX(주석 스크립트), IExpress(SED), WinZip
Self-Extractor, makeself. 설정은 네 갈래다: 압축 해제 뒤 일(실행, 기다림, 종료 코드, 정리, 바로가기, 환경
변수), 대상 위치와 파일(기본 경로, 묻기, 하위 폴더, 덮어쓰기), UI(제목, 시작 확인, 라이선스, 완료 메시지,
무인 모드, 배너, 언어별 문구), PE의 속성(아이콘, `VERSIONINFO`, manifest, 서브시스템). 명령줄로 받는 것은
대상 경로 `DIR`, `-y`, `--noexec`, `--keep`이고 `.aqr/sfx/`가 아니다. 두지 않기로 한 것은 8절, 코드 서명은
aqr 밖(signtool)의 일이다(서명은 SFX를 다 만든 뒤에).

## 2. 결정: 원본은 아카이브에, 컴파일된 설정은 실행 파일에

설정은 aqr의 옵션이 아니라 압축하는 입력의 최상위 `.aqr/sfx/` 안의 파일로 정한다.

- **원본**: `.aqr/sfx/` 아래 파일은 `sfx.toml` 포함 모두 그대로 아카이브에 들어간다. `aqr -d`로 풀고 다시
  `--format sfx`로 만들면 같은 SFX가 나오므로 설정을 실행 파일에서 되살리는 길이 필요 없다.
- **컴파일된 설정**: 압축할 때 aqr이 `sfx.toml`을 파싱해 PE의 속성(아이콘, `VERSIONINFO`, manifest,
  서브시스템)과 설정 blob(나머지, CBOR, 4절)으로 나눠 싣는다. 만들어 낸 뒤 아카이브 부분을 고쳐도 SFX의
  동작은 바뀌지 않는다. stub은 런타임에 `sfx.toml`을 읽지 않으므로 TOML 파서가 들어가지 않는다(stub 약
  0.78 MB, UPX로 0.35 MB).
- 설정이 없는 SFX는 전과 같다(현재 폴더, 있는 파일은 실패, stub 이미지는 그대로).

### 2.1 큰 설정은 파일로, 작은 설정은 toml로

긴 텍스트와 바이너리는 `.aqr/sfx/` 아래의 별도 파일(`license*.txt`, `banner.png`, `icon.ico`,
`manifest.xml`)로 두고 `sfx.toml`이 경로로 가리킨다. 아이콘과 manifest는 PE 리소스로 복사되고(탐색기가
읽는다), 라이선스와 배너는 아카이브에만 있다. 압축 해제보다 먼저 보여야 하지만 설정 blob에 복사하면
압축의 효과를 덜 본다. 그래서 **`.aqr/sfx/`를 아카이브의 첫 세그먼트에 두고 나머지는 그 뒤 세그먼트에
둔다**(`aqr_compress::Options::lead`; 첫 세그먼트에는 디렉터리 항목도 들어간다).

- 첫 세그먼트는 그 자체로 하나의 아카이브로 열린다(`aqr::open_upto`, 스펙 "`journal`"). stub은
  `.aqr/sfx/`의 파일을 첫 세그먼트에서만 찾는다(`aqr_sfx::config::read_files`). 설정 blob은 파일을 블록
  번호가 아니라 경로로 가리키고, 나중에 덧붙인 세그먼트가 같은 경로를 덮어써도 SFX가 보이는 것은 바뀌지
  않는다. 다른 도구로 아카이브를 고치는 경우에도 SFX의 동작이 안 바뀌게 하는 것이 이쪽이다.
- 비용: 메타데이터 블록 하나와 모듈 블록(데모 0.7 MB에서 68바이트, 2.2 MB 소스에서 132바이트; 모듈 블록은
  겹치지 않는다). `.aqr/sfx/`가 있을 때만 든다.

### 2.2 `sfx.toml`

- `mode`(`extract` | `run`)는 기본값의 묶음이다. `extract`는 `{cwd}`, `subdir = false`, `overwrite = "ask"`로
  풀고 끝낸다. `run`은 `{temp}`에 풀고 `overwrite = "always"`, 실행하고 기다리고 지우고 종료 코드를
  돌려준다(`cleanup`, `exit-code`는 `wait`를 따르고 `wait = false`에 켜면 오류). 나머지 키가 그 위에
  덮는다. `mode`가 없으면 `run.program`이 있을 때 `run`이다.
- `run.program`은 셸 문자열이 아니라 argv 배열이다. `CreateProcessW`로 셸 없이 실행하며 첫 원소는 아카이브
  안의 `.exe`(또는 `.com`)여야 한다. 그래서 `.bat`, `.cmd`, `.msi`는 실행할 수 없다(시스템의 `cmd`/`msiexec`를
  부르는 것은 8절의 "임의 명령"이다. 필요해지면 따로 허용하는 키를 검토한다).
- 경로 변수(`{temp}` `{here}` `{cwd}` `{programs}`, 인자와 환경 변수 값의 `{dest}`)는 고정된 몇 개이고 stub이
  해석한다(`%VAR%` 확장은 stub을 그 문법에 묶으므로 쓰지 않는다). `{temp}`는 임시 폴더 아래 새로 만든
  폴더이고, 정리는 stub이 만든 파일과 폴더만 지운다.
- 문구(`title`, `prompt`, `finish`, 바로가기의 `name`)는 문자열이거나 언어 태그별 표다. 고르는 순서는 태그
  전체, 첫 부분 태그, 언어 없음, `en`, 첫 항목이다. 라이선스와 배너는 파일 이름으로 고른다
  (`license.ko-KR.txt`, `license.ko.txt`, `license.txt`; `AQR_SFX_LANG`이 사용자 UI 언어를 덮는다).
- 바로가기(`[[shortcut]]`)는 파일을 쓴 뒤 실행 전에 `IShellLinkW`로 만든다. 파일이 남을 때만 뜻이 있으므로
  `run.cleanup`과 함께 쓰면 오류이고, `all-users = true`는 `pe.admin`(또는 `manifest.xml`)이 없으면 오류다.
  시험에서는 `AQR_SFX_SHORTCUTS`가 폴더를 대신한다.
- manifest: `requestedExecutionLevel`(`admin`), Windows 7–11 `supportedOS`, PerMonitorV2 DPI, Common Controls
  6, CLI일 때 `consoleAllocationPolicy`. `manifest.xml`이 있으면 그것을 쓰고 `admin`을 함께 주면 오류다.
  `pe.subsystem`의 기본은 `cli`다.
- `run` 모드는 임시 폴더에 풀 때 `.aqr/`를 풀지 않는다(`--noexec`이면 푼다).

## 3. 압축 시점의 검증

SFX를 실행할 때가 아니라 만들 때 오류가 드러나게 `compile`이 확인하고 실패시킨다: `run.program[0]`이
아카이브 안에 있는 상대 경로(`..` 없음), `ui.license`, `ui.banner`, `pe.icon`, `manifest.xml`이 `.aqr/sfx/`
안에 있고 아이콘이 올바른 `.ico`이며 배너의 magic이 PNG, BMP, JPEG, GIF 중 하나, 모르는 키 없음(오타가
조용히 무시되지 않게), `prompt`도 라이선스도 없는데 배너가 있으면 오류. `--format sfx`가 아닌데
`.aqr/sfx/`가 있으면 경고만 하고 일반 파일로 넣는다.

## 4. 설정 blob

- 형식은 CBOR 데이터 항목 하나다(RFC 8949). 아카이브의 선택적 메타데이터가 이미 CBOR이고, 읽는 코드
  (`aqr_archive::cbor`)는 stub에 이미 링크되어 있다. 키의 규칙은 스펙의 선택적 메타데이터를 따른다: 바깥은
  음수 정수 키의 맵, 모르는 키는 무시, 하위 호환이 깨지는 변경은 키를 더하는 대신 버전 키를 올리고 stub은
  모르는 버전이면 실패한다. 문자열은 UTF-8 text string이고 Win32에 넘길 때 UTF-16으로 바꾼다. 키의 CDDL은
  `crates/aqr-sfx/src/config.rs`에 있다.
- 번들의 엔트리가 아니라 번들과 아카이브 사이의 레코드 하나다: `"AQRSFXC1" le64:n cbor[n] le64:n
  "AQRSFXC1"`. 번들 엔트리는 모두 blob이라(`Blob::from_bytes`) 설정을 끼우려면 번들 형식을 바꿔야 했고,
  번들이 없는 SFX에 설정만 담은 번들을 붙이는 일도 생긴다. 앞뒤의 매직과 길이 덕에 stub은 번들 뒤에서 앞으로,
  또는 뒤에서 훑어 찾은 아카이브(aqr-ld merge의 stub, ELF 등) 바로 앞에서 거꾸로 읽는다. PE 리소스에 두는
  방법은 5절의 UPX 문제를 설정까지 떠안게 되어 쓰지 않았다.

## 5. PE 속성을 쓰는 일: UPX

stub이 UPX로 압축되어 있어 `.rsrc`를 나중에 바꿔 끼우기가 까다롭다. 후보(UPX에 `--compress-resources=0`을
주고 `.rsrc`를 끝 섹션에 두기, UPX 이전의 stub을 싣고 aqr이 압축하기, 빈 슬롯을 패치하기)는 모두 쓰지
않았고, **리소스를 마지막 섹션 뒤에 새 섹션으로 쓰고 데이터 디렉터리의 리소스 항목만 그쪽으로 돌린다**
(`crates/aqr-sfx/src/pe.rs`). UPX의 섹션(UPX0, UPX1, UPX2)은 한 바이트도 건드리지 않는다. stub에 있던 리소스는
데이터를 옮기지 않고 RVA 그대로 새 디렉터리에 다시 넣고(UPX가 실행할 때 푸는 리소스도 그대로 통한다), 새로
쓰는 종류(아이콘, 그룹 아이콘, `VERSIONINFO`, manifest)의 예전 항목만 빠진다. 남은 일은 탐색기에서 보이는
아이콘과 속성 창을 눈으로 확인하는 것이다(`crates/aqr/tests/sfx.rs`는 리소스를 읽고 UPX 압축 stub을 실행한다).

## 6. `.aqr/` 경로

- `.aqr/`는 입력의 최상위에서만 인식한다(하위 디렉터리의 `.aqr/`는 일반 데이터). 입력이 여러 개면 아카이브의
  루트에 오는 것 중 첫 번째만 쓰고 둘 이상이면 오류다.
- `.aqr/` 전체를 예약 경로로 선언한다(나중에 필터, 메타데이터가 생길 수 있다. `docs/hints-design.md`가 같은
  자리를 쓴다).
- `.aqr/sfx/`는 풀 때 일반 파일처럼 풀린다(SFX를 다시 만들 원본이므로). `run` 모드는 건너뛴다(2.2절).

## 7. SFX 속의 아카이브는 읽기 전용

aqr은 SFX 속의 아카이브를 열고 풀고 시험할 수 있지만 고치지는(`-a`, `-s`, `--delete`, `--compact`, 저널
되감기) 않는다. 압축기는 stub이 무엇을 읽는지, 덧붙인 세그먼트가 번들의 모듈 blob 없이도 풀리는지(새 모듈이면
인터프리터 blob이 필요하다), 고친 뒤에 서명이 유효한지 판단할 수 없다. 규칙은 "아카이브가 파일의 맨 앞에서
시작하지 않으면 고치지 않는다"(`crates/aqr/src/append.rs`)이고 SFX를 알아볼 필요가 없으며 다른 데이터 뒤에 붙은
아카이브에도 들어맞는다. 스펙의 "Appending to an archive"에도 같은 권고가 있다. 2.1절의 첫 세그먼트 규칙은 이
규칙과 별개로 둔다. 남은 일: 앞에 다른 바이트가 있을 때만 생기던 append.rs의 경로(섹터를 가로지르는 꼬리말을
지우고 새로 쓰기)는 이제 aqr에서 쓰이지 않는다. 지울지 정한다.

## 8. 보안과 평판

자동 실행 SFX는 악성 코드가 흔히 쓰는 형태라 stub 하나가 AV 휴리스틱에 걸리면 모든 SFX가 같이 걸린다.

- 실행 대상은 아카이브 안의 상대 경로뿐이다. 임의 명령, 압축 해제 전 실행(`Presetup`), 기존 파일 지우기
  (`Delete=`)는 두지 않는다. `run` 모드에서도 `--noexec`(풀기만)이 항상 통한다.
- `MB_GetString`을 `GetProcAddress`로 찾았을 때 Defender가 바로가기를 만드는 시험의 SFX를
  `Trojan:Win32/Bearfoos.A!ml`로 잡았다. stub의 버튼 문자열은 user32의 문자열 표(800번부터)를 `LoadStringW`로
  읽는다.
- **서명**: Authenticode 서명은 파일 끝에 인증서 표를 더하고 그 해시는 번들, 설정, 아카이브까지 덮는다. stub은
  자기 이미지 뒤에서 아카이브를 찾으므로 서명한 SFX도 돈다. 앞뒤에 다른 바이트가 있는 파일은 스펙 "Locating an
  archive"의 5단계(PE 인증서 표가 파일 끝이면 그 앞, 0 패딩은 건너뜀; `aqr_archive::locate`)가 받는다.
  덧붙이면 깨지므로 서명된 stub은 받지 않는다(pack.py, aqr의 팩 읽기). `crates/aqr/tests/sfx.rs`의
  `signed_afterwards`가 OpenSSL 인증서와 `signtool`로 시험한다(없으면 건너뜀). 서명은 `--run-after
  "signtool sign ... \"%AQR_OUTPUT%\""` hook(`crates/aqr/src/hooks.rs`)으로 잇는다. 다 만든 SFX에 서명이 없으면
  aqr이 알린다(stderr 한 줄, `-q`면 없음. 프런트엔드에는 `{"event":"notice","kind":"unsigned"}`,
  `crates/aqr/src/ipc.rs`; aqr-win은 경고 대화상자, 문구는 aqr-l10n의 `common.unsigned*`).

## 9. 남은 일

- 재부팅, 뒤이은 명령 등: 필요해질 때.
- stub 자신의 문구는 aqr-l10n의 `sfx` 섹션의 세 문장(라이선스 질문, `Extract to:`, 덮어쓰기 질문)뿐이고
  버튼은 운영체제의 것이다. 더 늘면 그곳에 더한다.
- 질문이 더 늘면(지금은 라이선스, 경로, 덮어쓰기, 배너가 stub의 메모리 대화상자) GUI blob으로 옮긴다
  (`encrypted-sfx-design.md` 4절). 그런 질문이 있는 설정이면 암호가 없어도 GUI blob을 붙이는 쪽으로 간다.
- 암호화된 SFX(`encrypted-sfx-design.md`).
- GUI(aqr-win)에서 SFX 설정(`.aqr/sfx/`)을 만들고 서명하는 길.
- 7절의 append.rs 경로 정리.
- 5절: 탐색기에서 아이콘과 속성 창을 눈으로 확인.

## 10. 구현 위치

`crates/aqr-sfx/src/config.rs`(설정, 레코드, `compile`: feature `encode`, aqr만), `pe.rs`(리소스, 서브시스템),
`crates/aqr/src/sfx.rs`(`settings`: 만든 아카이브에서 `.aqr/sfx/`의 파일만 디코딩해 컴파일하므로 원본이
아카이브에 있다는 것을 이 경로가 보장한다), stub `crates/aqr-sfx-shim`(`main.rs`의 명령줄 `[-y] [--noexec]
[--keep] [DIR]`, `run.rs`, `ui.rs`와 `banner.rs`, `shortcut.rs`, `folder.rs`). 덮어쓰기 정책은
`aqr_sfx::extract_with`(묻기는 y/n/always/never/quit; 파일 수정 시각은 아카이브대로 둔다. `newer`에 필요).
대화상자는 stub의 메모리 대화상자(user32)이고 글꼴은 메시지 상자와 같은 `lfMessageFont`다(`MS Shell Dlg`의
한글은 9pt에서 굴림의 비트맵이라 뭉개진다). 배너는 WIC(`CLSID_WICImagingFactory1`, Windows 7부터)로 디코딩해 창
DPI에 맞춰 Fant 보간으로 늘리고 실패하면 배너 없이 묻는다. 라이선스를 거절하거나 경로 묻기를 취소하면 종료
코드 2다. stub 크기 추이의 마지막: 781,312바이트(UPX로 354,304).

# 암호화된 자기 압축 해제 프로그램 (`--format sfx -P`)

지금 `aqr --format sfx`는 `-P`를 받지 않는다 (`crates/aqr/src/args.rs`: "the program does not
ask for one"). 자기 압축 해제 프로그램(이하 SFX)이 암호를 물을 방법이 없기 때문이다. 이 문서는
SFX가 암호를 GUI(탐색기에서 실행했을 때)와 CLI(콘솔에서 실행했을 때) 양쪽에서 받는 방법을 정한다.
암호화 자체는 스펙의 "Encrypted archives"를 따르고, 여기서는 되풀이하지 않는다.

## 1. 문제: 서브시스템은 실행 전에 정해진다

PE의 서브시스템(CONSOLE 3 / WINDOWS 2)은 로더가 프로세스를 띄울 때 이미 적용된다. 그래서 인자나
실행 환경을 보고 나서 고를 수 없다.

- CONSOLE: 탐색기에서 실행하면 OS가 새 콘솔을 만든다. 창이 번쩍이고, Windows 11에서 기본 터미널이
  Windows Terminal이면 터미널 창이 떴다 사라진다. cmd나 PowerShell은 종료를 기다리고, stdin과
  종료 코드도 정상이다.
- WINDOWS: 창은 번쩍이지 않는다. 대신 셸이 기다리지 않아 프롬프트가 먼저 돌아오고, `AttachConsole`로
  붙어도 stdin을 셸과 나눠 쓰게 된다. 콘솔에서 에코를 끄고 암호를 읽는 일은 사실상 불가능하다.

"옵션이 없으면 GUI, `-P`가 있으면 CLI"처럼 인자로 나누는 방법도 이 제약을 벗어나지 못한다.
인자는 프로세스가 뜬 뒤에야 읽을 수 있기 때문이다.

지금의 aqr-sfx-shim은 Rust 기본값인 CONSOLE이다 (`windows_subsystem` 지정 없음).

## 2. 결정: 서브시스템은 SFX를 만들 때 정하고, 암호 처리는 blob으로 붙인다

스텁은 하나만 빌드한다. `aqr --format sfx`가 스텁을 붙일 때 optional header의 `Subsystem` 필드를
패치해 GUI용과 CLI용을 만든다. 사용자 exe라 PE 체크섬은 검사되지 않으므로 고치지 않아도 된다.
7-Zip이 두 스텁(`7z.sfx` / `7zCon.sfx`)을 두고 만드는 쪽이 고르게 하는 것과 같은 선택이지만,
스텁을 한 벌만 싣는다는 점이 다르다.

| 아카이브 | 서브시스템 | 붙이는 것 |
|---|---|---|
| 암호 없음 | GUI 또는 CLI | shim만 (모듈 blob 번들은 지금과 같음) |
| 암호 있음, GUI | WINDOWS | shim + 복호화·GUI blob |
| 암호 있음, CLI | CONSOLE | shim + 복호화·CLI blob |

암호가 없으면 두 서브시스템의 차이는 창이 번쩍이는지(CLI를 더블클릭했을 때)와 출력이 보이는지뿐이다.

복호화 코드(scrypt, ChaCha20-Poly1305, HKDF, HMAC: `aqr-formula`의 `aqr::envelope`)와 암호를 묻는
UI는 blob으로만 싣는다. 그래서 암호화되지 않은 SFX의 스텁은 지금처럼 작게 유지된다
(0.67 MB, UPX로 0.30 MB). 복호화 코드는 작으므로 GUI blob과 CLI blob에 각각 넣는다. 중복되는
바이트보다 vtable이 하나로 끝나는 쪽이 낫다. blob은 지금의 다른 blob처럼 번들에 들어가 `aqr_blob::load`로
올라간다.

## 3. 복호화가 끼어드는 자리

암호화는 아카이브 전체를 감싸는 봉투다. 봉투를 열어 페이로드를 복호화한 뒤 그 페이로드를 아카이브로
연다 (`aqr::open`은 암호화된 아카이브에 `Failure::Decryption`을 돌려주고, 복호화된 페이로드는
`aqr::open_payload`가 연다). 따라서 세그먼트 단위의 훅은 필요 없고, blob의 일은 다음으로 끝난다.

1. `envelope::open`으로 헤더를 검사한다. 미러 헤더가 있으면 그 체크섬도 본다. 손상된 헤더는 암호를
   묻기 전에 걸러진다.
2. 암호를 묻는다. GUI는 대화상자, CLI는 에코를 끈 콘솔 입력이다.
3. `unlock`(scrypt)을 실행한다. 실패하면 다시 묻는 루프도 blob 안에서 돈다.
4. 페이로드를 복호화해 shim에 돌려준다. shim은 그것을 `open_payload`로 연다.

vtable은 대략 `decrypt(archive bytes, args) -> payload | failure` 하나다. 인자로 받은 암호(아래 4절)는
`args`로 넘긴다. 실패는 기존 blob처럼 종류를 붙여 C ABI를 넘긴다 (취소, 손상, 오류).

신경 쓸 점:

- scrypt는 기본 작업 인자(lg N = 18)에서 약 1초, 256 MiB가 들고, 여는 쪽 상한(22)에서는 4 GiB다.
  GUI는 이 동안 "확인 중"을 보여 줘야 하며, 메모리 부족은 별도의 실패로 알린다.
- 지금 shim은 자기 파일을 통째로 읽는다 (`std::fs::read`). 페이로드까지 통째로 복호화하면 메모리가
  두 배로 든다. 우선은 그대로 두고, 큰 SFX가 문제가 되면 청크 단위로 복호화하는 `envelope::Decrypting`
  (`aqr::Payload::Sealed`)을 쓰는 방향을 검토한다.
- shim이 지금 링크하는 것은 `envelope::is_encrypted`(매직 비교)뿐이다. `unlock` 이하가 실제로
  shim에서 빠져 있는지는 구현할 때 크기로 확인한다.

## 4. UI

GUI blob:

- `TaskDialog` 또는 대화상자 리소스로 암호를 입력받는다. 진행 표시는 선택 사항이다.
- Common Controls v6(`TaskDialog`, 비주얼 스타일)와 DPI awareness는 exe의 manifest와 그 activation
  context로 정해진다. 커스텀 로더로 올린 blob은 자기 manifest를 가질 수 없으므로, shim의 manifest에
  둘 다 넣는다. CLI 모드에서도 해가 없다. `TaskDialog`는 Vista부터 있으므로 Windows 7 대상으로도
  문제없다.
- blob이 가져오는 user32와 comctl32는 `load`가 시스템 DLL import로 바인딩한다. 다만 `CRT_TABLE`에
  없는 CRT 함수를 쓰면 `aqr-ld blob`이 실패하므로, 그럴 때는 표를 늘린다.

CLI blob:

- 콘솔 입력을 에코 없이 읽는다 (`SetConsoleMode`로 `ENABLE_ECHO_INPUT`을 끈다). stdin이 콘솔이
  아니면 한 줄을 그대로 읽는다.

양쪽 공통:

- SFX의 명령줄 `-P PASS`도 받아서, 자동화에서는 묻지 않게 한다. 이 경우 암호가 프로세스 목록에
  드러난다는 점을 도움말에 적는다. `-P`의 형식은 aqr의 `--passphrase-from`과 맞출지 정해야 한다.

shim 자체에도 필요한 것:

- WINDOWS 서브시스템에서는 stderr가 어디에도 이어지지 않아, 손상이나 쓰기 실패가 아무 표시 없이 끝난다.
  암호가 없는 GUI SFX도 마찬가지이므로, shim은 콘솔이 없을 때 오류를 `MessageBoxW`로 보인다.
  user32 하나면 되고 비용은 거의 없다. 덮어쓰기 확인이나 추출 위치 선택처럼 GUI에서 물어볼 것이
  늘어나면, 암호가 없어도 GUI blob을 붙이는 쪽으로 바꾼다.
- CLI SFX를 더블클릭했을 때 창이 번쩍이지 않도록, shim의 manifest에
  `<consoleAllocationPolicy xmlns="http://schemas.microsoft.com/SMI/2024/WindowsSettings">detached</consoleAllocationPolicy>`를
  넣는다 (Windows 11 24H2부터 지원; 그 이전 Windows는 이 요소를 무시한다). 부모에게 콘솔이 없으면
  `GetConsoleWindow()`가 NULL이므로 GUI에서 실행된 것으로 본다. 이 요소를 모르는 Windows에서는
  `GetConsoleProcessList`가 1을 돌려주면(새로 만든 콘솔) 같은 경우로 보고 `FreeConsole`한다.
  창은 잠깐 번쩍인다.

## 5. 패닉과 실패

shim은 `panic=abort`(3d539ed1)이다. blob의 패닉이 C ABI를 넘어 실패 종류로 돌아오는 기존 방식이
새 blob에도 그대로 통하는지 `pemerge-test.sh` 수준으로 확인한다. 특히 GUI 콜백(창 프로시저) 안에서
일어난 패닉은 OS 프레임을 넘어 풀리면 안 되므로, 콜백 경계에서 잡는다.

## 6. 아카이브 찾기 (스펙 문제)

스펙 "Locating an archive"는 암호화된 아카이브를 파일 맨 앞에서만 찾는다. 봉투에는 끝 표시가 없어
뒤에서 거슬러 찾을 수 없기 때문이다. 하지만 SFX에서는 암호화된 아카이브가 PE 이미지와 번들 뒤에 온다.

- shim은 문제없다. overlay 시작과 번들 길이로 위치를 정확히 알기 때문이다 (`overlay`, `bundle::len`).
  그래도 맨 뒤에서 거슬러 헤더 매직을 찾는 경로(`aqr-ld merge`로 만든 스텁)에서는
  `ENCRYPTED_HEADER_MAGIC`도 찾아야 한다.
- `aqr -d`나 `-t`로 암호화된 SFX를 여는 경우는 지금의 위치 찾기로는 실패한다. 둘 중 하나를 정해야 한다:
  스펙에 "PE overlay의 시작(SFX 번들이 있으면 그 뒤)"을 위치 후보로 더하거나, 암호화된 SFX는 aqr로
  열지 않는 것으로 하거나. 앞쪽이 낫다. 위치를 PE 헤더로 정확히 구할 수 있어 매직을 훑을 필요가 없다.

## 7. aqr 쪽에서 할 일

- `--format sfx`에서 `-P` 금지를 푼다. `-A`(미러)는 계속 금지한다. `-a`는 암호화된 아카이브에
  어차피 쓸 수 없다.
- GUI/CLI를 고르는 옵션을 둔다. 기본은 GUI다. SFX는 주로 더블클릭해서 쓰기 때문이다.
- `pack.py`가 복호화 blob 두 개를 만들어 `sfx.pack`에 넣는다. `sfx::make`는 고른 쪽 blob을 번들에
  넣고 `Subsystem` 필드를 패치한다.
- 릴리스 워크플로의 스모크 테스트에 암호화된 SFX(CLI, `-P PASS`)를 더한다. GUI 경로는 대화상자를
  거치지 않도록 `-P`로 시험한다.

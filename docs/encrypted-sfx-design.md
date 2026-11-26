# 암호화된 자기 압축 해제 프로그램 (`--format sfx -P`)

`aqr --format sfx`는 아직 `-P`를 받지 않는다(`crates/aqr/src/args.rs`: "the program does not ask for
one"). 자기 압축 해제 프로그램(이하 SFX)이 암호를 물을 방법이 없기 때문이다. 이 문서는 SFX가 암호를 GUI
(탐색기에서 실행)와 CLI(콘솔에서 실행) 양쪽에서 받는 방법을 정한다. 그중 서브시스템의 선택(2절)과 GUI가
없을 때의 알림(4절)은 SFX 설정(`sfx-config-design.md`)과 함께 구현했고, **복호화 blob과 `-P` 자체는
남았다.** 암호화는 스펙의 "Encrypted archives"를 따른다.

## 1. 문제: 서브시스템은 실행 전에 정해진다

PE의 서브시스템(CONSOLE 3 / WINDOWS 2)은 로더가 프로세스를 띄울 때 이미 적용되므로 인자나 실행 환경을
보고 고를 수 없다.

- CONSOLE: 탐색기에서 실행하면 새 콘솔이 생겨 창이 번쩍인다(Windows Terminal이 기본이면 터미널 창이 떴다
  사라진다). cmd나 PowerShell은 종료를 기다리고 stdin과 종료 코드도 정상이다.
- WINDOWS: 창은 번쩍이지 않지만 셸이 기다리지 않아 프롬프트가 먼저 돌아오고, `AttachConsole`로 붙어도
  stdin을 셸과 나눠 쓴다. 콘솔에서 에코를 끄고 암호를 읽는 일은 사실상 불가능하다.

## 2. 결정: 서브시스템은 SFX를 만들 때 정하고, 암호 처리는 blob으로 붙인다 [서브시스템 구현]

스텁은 한 벌만 빌드하고, `aqr --format sfx`가 optional header의 `Subsystem`을 패치해 GUI용과 CLI용을
만든다(`aqr_sfx::pe`, 설정의 `pe.subsystem`, 기본 `cli`). PE 체크섬은 검사되지 않는다. 7-Zip은 스텁 둘을
두고 만드는 쪽이 고르게 하는데, 여기서는 스텁이 한 벌이다.

| 아카이브 | 서브시스템 | 붙이는 것 |
|---|---|---|
| 암호 없음 | GUI 또는 CLI | shim만 (모듈 blob 번들은 지금과 같음) |
| 암호 있음, GUI | WINDOWS | shim + 복호화·GUI blob [남음] |
| 암호 있음, CLI | CONSOLE | shim + 복호화·CLI blob [남음] |

복호화 코드(scrypt, ChaCha20-Poly1305, HKDF, HMAC: `aqr-formula`의 `aqr::envelope`)와 암호를 묻는 UI는
blob으로만 싣는다. 그래야 암호 없는 SFX의 스텁이 작게 유지된다. 복호화 코드는 작으므로 GUI blob과 CLI blob에
각각 넣는다(vtable이 하나로 끝나는 쪽이 낫다). blob은 다른 blob처럼 번들에 들어가 `aqr_blob::load`로
올라간다.

## 3. 복호화가 끼어드는 자리 [남음]

암호화는 아카이브 전체를 감싸는 봉투다(`aqr::open`은 암호화된 아카이브에 `Failure::Decryption`을 돌려주고
복호화한 페이로드는 `aqr::open_payload`가 연다). 세그먼트 단위의 훅은 필요 없고, blob의 일은 다음이다.

1. `envelope::open`으로 헤더를 검사한다(미러 헤더의 체크섬도). 손상된 헤더는 암호를 묻기 전에 걸러진다.
2. 암호를 묻는다(GUI는 대화상자, CLI는 에코를 끈 콘솔 입력).
3. `unlock`(scrypt)을 실행한다. 실패하면 다시 묻는 루프도 blob 안에서 돈다.
4. 페이로드를 복호화해 shim에 돌려주고 shim이 `open_payload`로 연다.

vtable은 `decrypt(archive bytes, args) -> payload | failure` 정도이고 명령줄 암호(4절)는 `args`로 넘긴다.
실패는 기존 blob처럼 종류를 붙여 C ABI를 넘긴다(취소, 손상, 오류).

- scrypt는 기본 lg N = 18에서 약 1초, 256 MiB, 여는 쪽 상한(22)에서 4 GiB다. GUI는 "확인 중"을 보이고
  메모리 부족은 별도 실패로 알린다.
- shim은 자기 파일을 통째로 읽고(`std::fs::read`), 페이로드까지 통째로 복호화하면 메모리가 두 배다. 큰 SFX가
  문제가 되면 청크 단위의 `envelope::Decrypting`(`aqr::Payload::Sealed`)을 쓴다.
- `unlock` 이하가 shim에서 실제로 빠져 있는지는 구현할 때 크기로 확인한다(지금 링크하는 것은
  `envelope::is_encrypted`뿐).

## 4. UI

구현한 것(shim의 `src/ui.rs`): 콘솔이 없으면(표준 오류가 없으면) 오류와 질문을 `MessageBoxW`와 메모리에서
만든 대화상자로 보인다. CLI SFX를 탐색기에서 실행했을 때 창이 번쩍이지 않게 manifest에
`consoleAllocationPolicy = detached`를 넣고(Windows 11 24H2부터), 그것을 모르는 Windows에서는 시작할 때
자기 콘솔에 자기 프로세스뿐이고 표준 오류가 그 콘솔이면 `FreeConsole`한다(창은 잠깐 번쩍인다). 라이선스,
경로 묻기, 덮어쓰기 확인은 GUI blob이 아니라 shim의 대화상자로 했다(user32와 셸 COM만 쓰고 작다).

남은 것(암호용 blob):

- GUI blob: `TaskDialog` 또는 대화상자 리소스로 암호를 받는다. Common Controls v6와 DPI awareness는
  exe의 manifest로 정해지고 커스텀 로더의 blob은 자기 manifest가 없으므로 shim의 manifest에 이미 있다.
  blob의 user32와 comctl32 import는 `load`가 바인딩한다. `CRT_TABLE`에 없는 CRT 함수를 쓰면 `aqr-ld blob`이
  실패하므로 표를 늘린다. 암호만 받는 대화상자를 shim의 것으로 할지 blob으로 할지는 그때 정한다.
- CLI blob: `SetConsoleMode`로 `ENABLE_ECHO_INPUT`을 끄고 읽는다. stdin이 콘솔이 아니면 한 줄을 그대로
  읽는다.
- 공통: SFX의 명령줄 `-P PASS`도 받아 자동화에서 묻지 않게 한다(암호가 프로세스 목록에 드러남을 도움말에
  적는다). `-P`의 형식을 aqr의 `--passphrase-from`과 맞출지 정해야 한다.

## 5. 패닉과 실패 [남음]

shim은 `panic=abort`다. blob의 패닉이 C ABI를 넘어 실패 종류로 돌아오는 기존 방식이 새 blob에도 통하는지
`pemerge-test.sh` 수준으로 확인한다. GUI 콜백(창 프로시저) 안의 패닉은 OS 프레임을 넘어 풀리면 안 되므로
콜백 경계에서 잡는다.

## 6. 아카이브 찾기 (스펙 문제) [남음]

스펙 "Locating an archive"는 암호화된 아카이브를 파일 맨 앞에서만 찾는다(봉투에는 끝 표시가 없어 뒤에서
거슬러 찾을 수 없다; `aqr_archive::locate`의 1단계). SFX에서는 암호화된 아카이브가 PE 이미지와 번들 뒤에
온다.

- shim은 overlay 시작과 번들 길이로 위치를 알므로 문제없다(`overlay`, `bundle::len`). 다만 맨 뒤에서 헤더
  매직을 거슬러 찾는 경로(`aqr-ld merge`로 만든 스텁)는 `ENCRYPTED_HEADER_MAGIC`도 찾아야 한다.
- `aqr -d`/`-t`로 암호화된 SFX를 여는 경우는 지금의 위치 찾기로는 실패한다. 스펙에 "PE overlay의 시작
  (SFX 번들이 있으면 그 뒤)"을 위치 후보로 더하거나 암호화된 SFX를 aqr로 열지 않기로 해야 한다. 앞쪽이
  낫다(PE 헤더로 정확히 구한다). 서명된 SFX를 위한 5단계(PE certificate table)와 같은 자리다.

## 7. aqr 쪽에서 할 일 [남음]

- `--format sfx`에서 `-P` 금지를 푼다(`-A`는 계속 금지, `-a`는 암호화된 아카이브에 어차피 쓸 수 없다).
- GUI/CLI를 고르는 옵션(지금은 `sfx.toml`의 `pe.subsystem`뿐). 기본은 GUI로 둔다.
- `pack.py`가 복호화 blob 두 개를 만들어 `sfx.pack`에 넣고, `sfx::make`가 고른 쪽을 번들에 넣는다.
- 릴리스 워크플로의 스모크 테스트에 암호화된 SFX(CLI, `-P PASS`)를 더한다(GUI 경로도 `-P`로 시험).

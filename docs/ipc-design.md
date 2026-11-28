# 프런트엔드 프로토콜 통합 (`--ipc` 하나로)

aqr를 다른 프로그램이 부리는 길이 지금 세 개다. 이 문서는 그것을 하나의 프로토콜로 합치는 설계와,
그 프로토콜을 문서화된 공개 인터페이스로 둘지 `-Z` 뒤에 숨길지에 대한 결정을 적는다. 아직 구현 전이다.

## 1. 지금

| | `--ipc` (`crates/aqr/src/ipc.rs`) | `--session` (`session.rs`) | `--serve` (`serve.rs`) |
|---|---|---|---|
| 하는 일 | 명령줄 하나의 작업(압축, 해제, 검사, 변환, 추가)을 진행 상황과 질문을 주고받으며 실행 | 압축 대화상자가 떠 있는 동안 입력을 미리 훑고(`inventory`), `begin`으로 `--ipc`가 됨 | 아카이브를 열어 목록, 항목, 파일 바이트를 내줌(셸 확장, aqr-mount) |
| 메시지 | `event` 스트림, `ask`/`answer`(번호), `control` | `control` + `id`, `event` | `id`를 단 요청과 응답, `more`로 분할, `data:N` 뒤에 원시 바이트 |
| 수명 | 작업 하나 | 작업 하나 | 여러 아카이브, 여러 요청 동시 |
| 쓰는 곳 | aqr-win, aqr-mac (aqr-ipc의 `Job`) | aqr-win 압축 옵션 창 | aqr-win `--serve`, aqr-mount (aqr-ipc의 `Served`) |

문제:

- **틀이 셋이다.** 요청과 응답의 짝짓기가 `ask`/`answer` 번호, `control`의 `id`, 요청의 `id`로 제각각이고,
  분할 응답은 `--serve`에만, 바이너리는 `--serve`의 `read`에만 있다. 프런트엔드(aqr-ipc)도 `Job`과 `Served` 두 벌을 가진다.
- **바이너리 표시가 모호하다.** `--serve`는 최상위 `data`를 뒤따르는 바이트 수로 읽는데, 같은 이름이
  항목의 압축 크기(`-t -v`의 `data`)로도 쓰인다. 지금은 중첩 위치가 달라 우연히 안 부딪칠 뿐이다.
- **오류가 제각각이다.** `{"error":"..."}` 문자열 하나, 암호는 `encrypted`/`wrong` 같은 별도 필드.
- **`--session`은 `--ipc`의 특수한 서막일 뿐이다.** 같은 프로세스에서 "먼저 조사, 나중에 실행"을 하려는 것인데,
  이것이 별도 옵션이 된 것은 `--ipc`가 요청을 받는 틀이 아니었기 때문이다.

## 2. 결정: 문서화한다 (`-Z`로 숨기지 않는다)

- **aqr-mount가 이 프로토콜의 사용자다.** aqr-mount는 GPL-3.0으로 따로 배포되고 aqr와는 이 프로토콜로만 이야기한다.
  문서화된 공개 인터페이스로 통신하는 별개의 프로그램이라는 사실이 라이선스 경계를 분명하게 한다.
  내부용 숨은 옵션이면 그 경계가 흐려진다.
- **프런트엔드가 이미 여럿이다.** aqr-win, aqr-mac, aqr-mount(Windows, Linux), 앞으로 macOS 마운트.
  모두 같은 저장소에 있지만, 문서 없이 코드끼리만 맞추는 인터페이스는 세 벌이 된 지금처럼 다시 갈라진다.
- **안정성 약속은 버전으로 관리한다.** `hello`로 버전과 기능을 협상하고(§3.2), 1.0 전에는 판이 바뀔 수 있다고
  명시한다. aqr와 동봉 프런트엔드는 늘 함께 배포되므로 하위 호환 부담은 크지 않다.
- **`-Z`는 값과 스위치의 자리다**(`aqr-common::zopt`: 상수였던 것을 바꾸는 개발자 옵션). 프로그램 사이의 계약을 두기에는 맞지 않는다.

문서의 자리는 매뉴얼 페이지 `man/aqripc.7`이다. `aqrsfx.5`, `aqrattributes.5`처럼 규범적인 문서로 두고, `--help`의
`--ipc` 한 줄과 `aqr.1`이 그것을 가리킨다. 이 설계 문서는 근거와 남은 일만 남긴다.

## 3. 설계

### 3.1 전송과 틀

- `aqr --ipc`의 표준 입력과 출력. 다른 인자는 받지 않는다(작업의 인자는 요청으로 온다, §3.4). 표준 오류는 사람을 위한
  로그일 뿐 프로토콜이 아니다. 표준 입력이 닫히면 모든 것을 취소하고 끝난다(지금과 같다).
- **메시지 = 머리 줄 하나 + 선택적 덧붙임 바이트.** 머리 줄은 JSON 객체 하나에 `\n`. 최상위 `"blob": N`이 있으면
  줄바꿈 바로 뒤에 원시 바이트 N개가 따른다. `blob`은 최상위에서만 예약된 이름이라 본문의 어떤 필드와도 부딪치지 않는다.
  양방향 모두 같은 틀이다(지금은 aqr→프런트엔드만 쓰지만, 스트림으로 파일을 받아 추가하는 일 등을 막지 않는다).
- base64를 쓰지 않는다. 바이트는 그대로 흐르고, 읽는 쪽은 줄을 읽고 `read_exact(N)` 한 번이면 된다.
- 한 메시지는 한 번에 쓴다(쓰는 스레드 하나, 또는 잠금 하나). 그래야 동시에 도는 응답의 머리와 바이트가 섞이지 않는다.

### 3.2 hello

첫 메시지는 프런트엔드의 `hello`, aqr는 다른 무엇보다 먼저 답한다.

```text
> {"hello":2,"answers":["overwrite","passphrase"],"chunk":4194304}
< {"hello":2,"ops":["run","inventory","open","list","stat","read","close","journal","progress","trim","cancel"],"chunk":1048576}
```

- `hello`는 판 번호(통합판이 2). aqr는 자기가 아는 가장 높은 판 이하로 답하고, 그보다 낮은 프런트엔드는 거절한다.
- `answers`: 프런트엔드가 답하는 질문의 종류(지금 `--ipc`의 그것). 모르는 질문은 묻지 않고 질문이 없을 때처럼 처리한다.
- `ops`: aqr가 받는 요청. 프런트엔드는 이것으로 기능을 확인하고, 없는 요청은 보내지 않는다.
- `chunk`: 한 덧붙임의 상한. 프런트엔드가 바라는 값을 보내고 aqr가 실제 값을 답한다(기본 1 MiB). §3.7 참고.

### 3.3 요청, 부분 응답, 최종 응답

모든 상호작용은 요청 하나에 응답 여럿이다.

```text
> {"q":5,"op":"read","archive":1,"path":"a/b.bin","offset":0,"len":3145728}
< {"r":5,"more":true,"blob":1048576}   + 1 MiB
< {"r":5,"more":true,"blob":1048576}   + 1 MiB
< {"r":5,"blob":1048576}               + 1 MiB  (마지막: more 없음)
```

- 요청은 `q`(보내는 쪽이 매기는 번호)와 `op`. 응답은 `r`(그 번호)이고, `more:true`인 동안 이어지며,
  `more`가 없는 응답이 마지막이다. 번호 공간은 방향마다 따로다. 프런트엔드의 `q:5`와 aqr의 `q:5`는 다른 요청이다.
- **양쪽 다 요청한다.** aqr의 질문(덮어쓸까, 암호는)은 aqr→프런트엔드 요청이다.

  ```text
  < {"q":1,"op":"ask","job":3,"kind":"overwrite","path":"x.aqr"}
  > {"r":1,"value":true}
  ```

  지금의 `ask`/`answer`가 이 일반 틀의 한 경우가 된다.
- 요청은 순서와 무관하게 동시에 처리된다. 응답 순서는 요청 순서를 따르지 않는다(지금 `--serve`와 같다).
- **오류**는 최종 응답의 `error` 객체다: `{"r":5,"error":{"kind":"not-found","message":"..."}}`.
  `kind`는 기계가 읽는 낱말(`not-found`, `not-a-directory`, `encrypted`, `wrong-passphrase`, `changed`, `busy`, `failed`, …),
  `message`는 영어 문장으로, 모르는 `kind`를 받은 프런트엔드가 그대로 보여 준다. 지금의 `encrypted`/`wrong` 필드는
  `kind`로 흡수된다.
- **취소**는 요청 하나로 통일한다: `{"q":9,"op":"cancel","of":5}`. 무엇이든(긴 `read`, `inventory`, `run`) 취소된 요청은
  `error.kind` `cancelled`로 끝난다. `run`의 "있는 그대로 끝내기"(지금 `finish`)는 `{"op":"cancel","of":N,"finish":true}`.

### 3.4 연산

**작업** (지금 `--ipc`와 `--session`):

- `run` `{"args":[...]}`: 명령줄 그대로의 인자로 작업 하나. 응답 스트림이 곧 지금의 `event`다:
  `{"r":N,"more":true,"event":"begin",...}`, `progress`, `output`, `notice`, `skipped`, … 그리고 마지막에
  `{"r":N,"event":"end",...}` 또는 `error`. 진행 중 질문은 위의 `ask` 요청으로 오며, `job`에 이 요청의 번호를 단다.
- `inventory` `{"args":[...]}`: 지금 `--session`의 그것. 부분 응답으로 `files`, `bytes`, `walked`가 오고 `done`으로 끝난다.
  같은 입력의 `inventory`가 있으면 뒤의 `run`이 그 결과를 쓴다(지금 `session::cached`). `--session`이라는 옵션은 사라진다.
- 한 프로세스에서 `run`은 한 번에 하나다. 스레드 풀, 신호, 중단(`crate::stop`)이 프로세스 전역이기 때문이다.
  두 번째 `run`은 `busy`로 거절한다. 작업마다 프로세스를 띄우는 지금 방식은 그대로 쓸 수 있다.

**아카이브 탐색** (지금 `--serve`): `open`, `list`, `stat`, `read`, `close`, `journal`, `progress`, `trim`.
뜻은 지금과 같고 틀만 바뀐다.

- `open`은 핸들(`archive`)을 준다. `list`는 `more`로 나뉜 응답(지금 `LISTED`개씩).
- `read`는 `len`이 `chunk`보다 커도 되고, 응답이 `chunk`씩 나뉘어 온다(§3.7). 지금은 1 MiB마다 요청을 다시 보내야 한다.
- 한 프로세스가 탐색과 작업을 함께 해도 된다. 다만 aqr-win이 그렇게 쓸 이유는 지금 없다.

### 3.5 이름과 경로

- 경로는 JSON 문자열이다. 아카이브 안 경로는 `/`로 잇고(경로 구분자 정책), 파일시스템 경로는 OS의 표기를 따른다.
- **UTF-8로 나타낼 수 없는 경로**(Windows의 짝 없는 서로게이트, Unix의 임의 바이트)는 문자열 대신
  `{"raw":"<16진>"}`으로 쓴다. Windows는 UTF-16 단위의 리틀 엔디언, Unix는 바이트 그대로다. 드물기 때문에
  보통 경로의 비용을 늘리지 않는다. 지금은 이런 경로가 손실 변환(`to_string_lossy`)되거나 실패한다.

### 3.6 진행과 사건

`progress`는 지금처럼 초당 4번 남짓으로 제한한다. 부분 응답이 하나 늘어날 뿐, 형식은 지금 `event`와 같다.
프런트엔드가 덜 원하면 `run`에 `"progress":MS`로 간격을 줄 수 있게 한다(Finder 진행처럼 드물게 보이는 경우).

### 3.7 효율

- **바이트는 복사 한 번.** aqr는 캐시(평가한 리프)에서 덧붙임으로 바로 쓰고, 프런트엔드는 받은 버퍼를 그대로 넘긴다.
  JSON 파싱은 머리 줄 몇십 바이트뿐이다.
- **chunk와 동시성의 균형.** 덧붙임 하나가 너무 크면 그동안 다른 응답(다른 파일의 작은 `read`, `list`)이 기다린다.
  그래서 한 덧붙임은 `chunk`로 묶고, 큰 `read`는 여러 부분 응답으로 나누어 사이사이 다른 응답이 끼어들게 한다.
  기본 1 MiB, 프런트엔드가 원하면 4 MiB까지 늘린다(aqr-mount처럼 한 프로그램이 쓰는 경우).
- **왕복을 줄인다.** 지금 aqr-mount는 3 MiB 파일 하나에 `read`를 세 번 보낸다. 통합판에서는 한 번이다.
- **파이프 자체**는 Windows에서도 GB/s 단위라 병목이 아니다. 병목은 평가(압축 해제)이고, 그쪽은 이 설계와 무관하다.
- 텍스트 틀의 비용은 메시지당 머리 줄 하나다. 진행 사건(초당 4번)과 목록(4096개씩)에서는 무시할 만하다.

### 3.8 예

```text
> {"hello":2,"answers":["overwrite","passphrase"]}
< {"hello":2,"ops":[...],"chunk":1048576}
> {"q":1,"op":"inventory","args":["-c","--","D:\\a"]}
< {"r":1,"more":true,"files":120,"bytes":3400000,"walked":false}
< {"r":1,"files":1400,"bytes":61000000,"walked":true,"done":true}
> {"q":2,"op":"run","args":["-c","-l","7","-o","D:\\a.aqr","--","D:\\a"]}
< {"r":2,"more":true,"event":"begin","mode":"compress","inputs":["D:\\a"],"output":"D:\\a.aqr"}
< {"q":1,"op":"ask","job":2,"kind":"overwrite","path":"D:\\a.aqr"}
> {"r":1,"value":true}
< {"r":2,"more":true,"event":"progress","analysis":[100,1400],...}
> {"q":3,"op":"cancel","of":2,"finish":true}
< {"r":3}
< {"r":2,"event":"stopped","kept":{"files":800,"bytes":31000000}}
```

## 4. 옛 것과 새 것

| 지금 | 통합판 |
|---|---|
| `aqr --ipc ARGS…` | `aqr --ipc` + `run {"args":ARGS}` |
| `aqr --session`, `inventory`, `begin` | `inventory`, `run` (같은 프로세스) |
| `aqr --serve` | `aqr --ipc` + `open`/`list`/`read`/… |
| `{"event":...}` | `run`의 부분 응답 |
| `{"ask":N}` / `{"answer":N}` | aqr→프런트엔드 요청 `ask` / 그 응답 |
| `{"control":"cancel"\|"finish"}` | `cancel {"of":N[,"finish":true]}` |
| `--serve`의 `{"id":N,...}` | `{"q":N,"op":...}` / `{"r":N,...}` |
| `"data":N` + 바이트 | `"blob":N` + 바이트 |
| `{"error":"..."}`, `encrypted`, `wrong` | `{"error":{"kind","message"}}` |
| `read`는 1 MiB까지 | `read`의 `len` 제한 없음, `chunk`씩 나뉜 응답 |

## 5. 옮기는 순서

1. **aqr:** 공통 틀을 만든다. 메시지 읽기와 쓰기(`blob` 포함), 양방향 요청 번호, 부분 응답, `cancel`, `error`를 담은
   `crates/aqr/src/ipc/`의 `wire`다. `serve.rs`의 요청 처리를 이 위로 옮긴다. 가장 요청/응답답기 때문에 먼저 옮긴다.
2. **aqr:** `run`과 `inventory`를 같은 틀에 올린다. 지금의 `event` 생성과 질문(`ipc.rs`)은 내용 그대로 두고 감싸는 것만 바꾼다.
   `session.rs`의 캐시는 `inventory`/`run` 사이로 옮긴다.
3. **aqr-ipc:** `Job`과 `Served`를 하나의 클라이언트로 합친다. 요청을 보내 응답 스트림을 받는 것 하나에,
   `run`과 `read` 같은 편의 함수를 얹는다. aqr-win, aqr-mac, aqr-mount는 그 위로 옮긴다.
4. **옵션:** `--session`과 `--serve`를 없앤다. aqr와 프런트엔드가 늘 함께 배포되므로 옛 이름을 남겨 두지 않는다.
   `hello` 1을 보내는 프런트엔드는 "판이 다르다"는 오류로 거절한다.
5. **문서:** `man/aqripc.7`(규범), `aqr.1`의 `--ipc`, `--help` 한 줄, 그리고 이 문서를 근거와 남은 일만 남기게 줄인다.

테스트: `crates/aqr/tests/serve.rs`, `ipc.rs`, `session` 관련 테스트를 새 틀로 옮긴다. 틀 자체(`blob`, 분할, 양방향 번호,
취소, 오류)는 `wire`의 단위 테스트로 둔다.

## 6. 열린 질문

- **`run` 여러 개.** 한 프로세스에서 작업을 동시에 돌릴 일이 생기면 풀과 중단을 작업별로 나눠야 한다. 지금은 `busy`로 막는다.
- **프런트엔드→aqr 바이트.** 틀은 허용하지만 쓰는 요청이 아직 없다. 후보는 스트림을 받아 아카이브에 추가하는 것이다.
- **판 2의 안정성.** 1.0 전까지는 판 번호를 올려 바꾼다. 그 뒤에는 기능을 `ops`와 필드 추가로만 늘리고,
  지우거나 뜻을 바꿀 때만 판을 올린다.

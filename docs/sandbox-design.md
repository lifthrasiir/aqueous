# Aqueous 모듈 샌드박스

디코더가 아카이브의 Wasm 모듈을 실행할 때 엔진 자체의 격리 말고 운영체제 수준의 격리를 하나 더 두는 방법이다.
Windows, Linux, macOS에서 구현했고, 이 문서는 남은 설계 근거와 측정, 남은 일을 둔다. 상태 표기는
`xform-design.md`와 같다(**[확정]**, **[잠정]**, **[제안]**).

코드(옮겨졌다: 전에는 aqr-decompress에 있었다):

- `crates/aqr-wasmrt/src/sandbox/`: `mod.rs`(`Sandbox`, `SUPPORTED`, 브로커 쪽 워커 관리), `proto.rs`(메시지,
  `layout`), `worker.rs`(`worker_main`: 초기화, 봉쇄, 요청 루프), `os/`(`windows.rs`, `linux.rs`, `macos.rs`,
  공유하는 `unix.rs`, `unsupported.rs`: `spawn`, `Child`, `lockdown`, arena). `Sandboxed`(알려진 모듈은 네이티브,
  나머지는 워커)는 aqr-decompress에 있다. 작업 메모리 CoW는 `aqr-wasmrt/src/cow/win.rs`(Windows).
- `crates/aqr-decompress/src/bin/worker.rs`: 참고용 helper `aqr-decompress-worker`.
- 시험(aqr-decompress): `tests/wasm.rs`(모든 load/apply/decode를 샌드박스에서도 실행해 결과와 실패가 같은지),
  `tests/sandbox.rs`(봉쇄: 파일·프로세스·네트워크 거부와 기동 실패; `Sandbox::probe`), `tests/sandbox_memory.rs`.
- aqr CLI: `crates/aqr/src/decompress.rs`의 `Decoder`. 봉쇄할 수 있는 시스템에서는 기본으로 알려지지 않은
  모듈(`-Z wasm`이면 모두)을 아카이브마다 하나의 `Sandbox`에서 돌리고(첫 load 때 만든다), 워커는 aqr 자신을 숨은
  인자 `--sandbox-worker`로 다시 실행한 것이다. `-Z no-sandbox`는 in-process. 봉쇄할 수 없는 시스템에서는
  알려지지 않은 모듈이 `-Z no-sandbox` 없이는 실패한다.

## 0. 전제 [확정]

- 엔진에서도 탈출 버그는 나올 수 있다. 임의의 모듈(`xform-design.md` §5.7의 수준 2)을 실행하는 디코더는 이를
  전제로, 모듈 실행을 OS가 봉쇄한 별도 프로세스로 옮겨 탈출한 코드가 파일·네트워크·다른 프로세스에 닿지 못하게 한다.
- 라이브러리의 동작은 바꾸지 않는다. 샌드박스는 호출자가 고르는 runner로만 제공하고, 결과는 대응하는 runner와
  같아야 하며 실패도 같아야 한다(예외: 워커를 띄우지 못했거나 예기치 않게 끝나면 resource 실패).
- 알려진 모듈은 네이티브(`Sandboxed`)라 흔한 경우의 비용은 0이다.
- 봉쇄할 수 없는 시스템에서는 워커를 띄우지 않는다(봉쇄되지 않은 워커는 프로세스를 나누는 것만으로는 아무것도 막지 못한다).

## 1. 구조 [확정]

- 브로커(디코더)와 워커(`Loaded::apply` 단위의 일을 받는 프로세스). 경계가 `Runner`/`Loaded`이므로 나머지 코드는 그대로다.
  모듈은 사실상 순수 함수 `(params, resources, inputs, sizes, limit) → outputs | Failure`라 워커에는 표준 입출력 말고
  자원이 필요 없다.
- 워커 프로그램은 호스트가 정한다(`Sandbox::new(program, args)` 또는 `current_exe(args)`; 워커 쪽은 `worker_main()`).
  build.rs로 helper를 빌드하지 않는다(target 디렉터리를 공유 못해 엔진을 두 번 빌드하고, 만든 바이너리를 런타임에 둘
  공식 수단이 없다; 임베드는 Windows·macOS에서 임시 파일 실행 문제. `-Z bindeps`가 안정화되면 다시).
- 워커는 `Sandbox` 안에서 재사용하고(놀고 있는 것을 꺼내고 없을 때만 띄운다), 받은 모듈을 해시별로 들고, 다른
  `Sandbox`와 공유하지 않는다. 탈취된 워커의 영향은 같은 `Sandbox`의 다음 결과뿐이다(아카이브마다 만들면 그 아카이브로 한정).
- 워커는 받을 수 있는 것(pool 스레드, 엔진, 컴파일 스레드, 지연 로드 시스템 라이브러리)을 모두 받은 뒤 스스로 봉쇄한다
  (`os::lockdown`). 봉쇄 전에 읽는 것은 HELLO와 스레드 수뿐이다. Windows는 요청 루프가 메인 스레드에서 돌고(초기화의
  impersonation 토큰은 `RevertToSelf`로 내린다), 모듈 실행은 스택이 큰 pool 스레드로 넘긴다. 처음에는 요청 루프가 pool
  스레드에 있어 `RevertToSelf`를 빼도 시험이 통과했다: 탈출한 코드가 메인 스레드를 가로채면 토큰을 쓸 수 있다.
- 메시지는 워커의 stdin/stdout. 브로커는 워커를 신뢰하지 않는다: 길이는 할당 전에 상한(출력은 `sizes`, 메시지 1 MiB)과
  대조하고, 출력 개수·크기는 선언과 정확히 같아야 하며, 위반한 워커는 버리고 resource 실패로 처리한다.
- 입출력 합 64 KiB 이상인 `apply`는 워커별 공유 영역(arena)에 둔다(메시지에는 길이만). 크기는 모자랄 때만 2의
  거듭제곱(최소 1 MiB)으로 새로 만들어 넘기고 이전 것은 양쪽에서 해제한다(256 MiB 초과는 그 요청 하나에만). 배치는
  길이로 정해진다(`proto::layout`, 입력들 다음 출력들, 각각 64 KiB 정렬). 워커는 arena의 입력을 읽고 출력을 직접 쓴다
  (`Shared::apply_into`). 브로커는 출력을 정확히 한 번만 읽어 복사한다(탈취된 워커가 계속 고쳐 쓸 수 있으므로 double
  fetch를 하지 않는다). arena는 워커가 크기를 바꿀 수 없어 브로커가 SIGBUS를 맞지 않는다. 터무니없는 크기는 만들기
  전에 메모리 한도 실패(`Shared::check`). arena를 못 만들면 메시지로 보낸다(결과는 같다).
- 워커의 환경 변수는 비우고(Windows는 `SystemRoot`만), stderr는 버리고, 현재 디렉터리는 시스템 루트.

## 2. OS별 봉쇄

### 2.1 Windows [확정]

- Chromium 렌더러와 같다. 토큰: 사용자의 모든 SID를 deny-only, 특권 제거, restricting SID는 NULL SID, integrity low.
  job: 자식 금지(`ACTIVE_PROCESS` 1), UI 제한 전부, 닫히면 종료, 처리되지 않은 예외에서 종료, 요청마다 정하는 메모리
  한도(commit 기준: 요청의 한도 + 전송 바이트 + 1 GiB; 엔진의 주소 예약은 commit이 아니라 부딪히지 않는다).
  mitigation: Win32k 금지, strict handle check, extension point·폰트·원격·low-label 이미지 금지, system32 우선.
  상속 핸들은 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 stdin/stdout 둘뿐, 콘솔 없음.
- 워커는 restricted 토큰(사용자의 모든 SID와 RESTRICTED를 restricting SID로 둔 low integrity)을 impersonate한 채 시작해
  시스템 라이브러리를 로드하고 `RevertToSelf`로 내린다. 프로세스 토큰이 restricted가 아니면 서비스를 거부한다
  (restricted 프로세스는 restricted 토큰만 impersonate할 수 있다: 아니면 `STATUS_BAD_IMPERSONATION_LEVEL`).
- arena는 pagefile 기반의 이름 없는 section을 `DuplicateHandle`로 넘긴다.
- ACG(동적 코드 금지)는 걸 수 없다(Cranelift가 코드를 실행 가능하게 만든다; 컴파일을 다른 프로세스로 빼야 하는데 비용에 비해 얻는 것이 적다).
- **[제안]** CET 강제 모드는 확인하지 않았다(트랩 복귀에서 RIP를 바꾸므로 shadow stack과 충돌할 수 있다 *(확인 필요)*).
  초기화 뒤에 integrity를 untrusted로 더 내리는 것(delayed integrity)도 안 했다.

### 2.2 Linux

- **[확정]** `PR_SET_NO_NEW_PRIVS`, fd 4 이상 전부 닫기(`close_range`; fd 3은 arena의 소켓), seccomp 허용 목록을 `TSYNC`로.
  허용: 메모리 관리, `futex`·`sched_yield`, `CLONE_THREAD`가 있는 `clone`(`clone3`는 `ENOSYS`로 폴백시킨다), 시그널(트랩 처리),
  시간, `getrandom`, 자기 프로세스로의 `tgkill`, `PR_SET_NAME`만의 `prctl`, 읽기 전용 `prlimit64`, fd 3의 `recvmsg`, 열린 fd의
  `read`·`write`·`fstat`·`close`. 그 밖은 `EPERM`, 아키텍처가 다르거나 x32면 프로세스를 죽인다.
- **[확정]** arena는 memfd를 `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`로 봉인해 `SCM_RIGHTS`로 보낸다(안 봉인해도 워커는 seccomp 때문에 `ftruncate`를 못 부른다).
- **[잠정]** OS 수준의 메모리 한도가 없다(워커 안의 `ResourceLimiter`는 동작). 남은 위험은 탈취된 워커의 메모리 고갈(DoS).
  `RLIMIT_DATA`는 스레드 스택과 엔진이 실제로 쓰는 부분까지 세어 여유분을 정하기 어렵고, cgroup v2 `memory.max`는
  delegation이 늘 되어 있지는 않다.
- **[제안]** 심층 방어로 user/pid/net/mount 네임스페이스나 Landlock(비특권 user ns는 Ubuntu 24.04+ 등에서 막혀 있으므로 seccomp만으로도 동작해야 한다).
  부모가 죽은 뒤의 정리는 파이프 EOF에 맡긴다(`PR_SET_PDEATHSIG`는 띄운 스레드가 끝날 때 신호를 보내 스레드가 오가는 호스트에서 엉뚱하게 죽는다).

### 2.3 macOS [확정]

- fd 4 이상 닫기(`proc_pidinfo(PROC_PIDLISTFDS)`) 뒤 Seatbelt `(version 1) (deny default)`를 `sandbox_init_with_parameters`로
  건다(deprecated이지만 Chromium·Firefox가 쓴다). 허용하는 것이 없다: 프로필은 파일·소켓·프로세스·Mach lookup·sysctl만 검사하고
  메모리·스레드·시그널·이미 열린 fd는 검사 대상이 아니다. 위반 로그가 탈출의 흔적이므로 `no-log`를 쓰지 않는다.
- 엔진의 Mach exception port 트랩 처리는 샌드박스 아래에서도 그대로 동작한다(`macos_use_mach_ports(false)` 불필요; 서명 안 했거나
  ad-hoc 서명한 실행 파일에서 확인).
- 메모리 한도는 브로커의 워커별 감시 스레드가 10 ms마다 `proc_pid_rusage`의 `ri_phys_footprint`를 보고 넘으면 죽인다(한도는 Windows와
  같다). kill은 pid가 아니라 `std::process::Child`로 보낸다(pid 재사용으로 엉뚱한 프로세스를 죽이지 않는다). 폴링이라 10 ms 사이에 넘을 수
  있으나 DoS 방지로 충분하다(커널이 강제하는 수단은 비공개이거나 특권이 필요하다).
- arena는 `shm_open` 직후 `shm_unlink`하고 fd를 소켓으로 보낸다(macOS 공유 메모리는 `ftruncate`를 한 번만 받는다: 키울 때는 새로 만든다).
- **[제안]** hardened runtime으로 서명한 실행 파일에서는 JIT entitlement(`allow-jit` 또는 `allow-unsigned-executable-memory`)가 필요할 것이다 *(확인 필요)*: 배포 형태가 정해질 때.

### 2.4 그 밖 [확정]

`os/unsupported.rs`가 워커 시작을 거부한다(`Unsupported`), 그래서 `Sandbox`는 resource 실패.

## 3. 검증과 측정

- 동치성(`tests/wasm.rs`)과 봉쇄(`tests/sandbox.rs`)는 Windows, Linux(x86_64, aarch64), macOS에서 통과했고, 봉쇄 장치(`RevertToSelf`,
  seccomp, Seatbelt, macOS의 메모리 감시 kill)를 빼면 시험이 실패하는 것도 확인했다. Windows에서는 파일·프로세스가 ACCESS_DENIED,
  소켓은 `WSAStartup`부터 실패, Linux·macOS는 넷 다 `EPERM`.
- 비용(release, 2026-10, Windows와 macOS Apple Silicon; Linux는 재지 않았다):

  | 항목 | Windows | macOS |
  |---|---|---|
  | 워커 기동(spawn, 봉쇄, 워밍업) | 약 7 ms(처음만 약 160 ms) | 약 7 ms(처음만 약 140 ms) |
  | 모듈 컴파일(워커 × 모듈마다) | flate 84 ms, jpeg 35 ms, mp3 16 ms | |
  | `apply` 왕복 | 약 36 µs 추가 | 약 11 µs 추가 |
  | 바이트 전달, arena | 1 MiB + 1 MiB에 0.07 ms, 64 + 64 MiB에 5.7 ms 추가 | 0.05 ms, 2.5 ms 추가 |

  arena 전에는 파이프로 1 MiB + 1 MiB에 0.4 ms(Windows)·0.19 ms(macOS) 추가였다(바이트당 비용은 모듈의 속도에 비례: 1–2 GB/s 모듈에서
  25–50%; arena로는 MiB당 0.30 → 0.04 ms). 끝에서 끝: 48 MB 텍스트를 gzip -6한 아카이브(출력 13.9 MB, Windows 16스레드)를 `Sandbox`
  0.63–0.65 s, in-process Wasm 0.59–0.61 s, 네이티브 0.22–0.24 s. 차이의 대부분은 고정비(기동과 flate 컴파일 약 90 ms)다.
  Windows의 arena 비용(MiB당 0.044 ms)은 macOS(0.02 ms)의 두 배쯤인데 새로 commit한 페이지의 첫 접근 비용으로 보인다 *(확인 필요)*.

## 4. 다음에 할 일

### 4.1 바이트당 비용

- **[확정, 구현]** Windows의 task 메모리 CoW(샌드박스만의 일은 아니다): 작업 메모리를 placeholder 위에 만들고 스냅샷 section의 view로 바꾼다
  (`cow/win.rs`; 같은 타입의 메모리를 정의해 export하는 작은 모듈을 같은 Store에 instantiate해 `aqr_memory` import로 넘긴다. 호스트가 만드는 메모리는
  `MemoryCreator`를 거치지 않고 예약의 일부만 풀 수도 없어 Linux처럼 덮어쓸 수 없다. 모듈의 import를 정의로 고쳐 쓰는 길은 메모리 인덱스가
  밀려 코드 전체를 다시 인코딩해야 해서 버렸다). section이나 view를 못 만들면 memcpy로 돌아간다. 0인 페이지는 건너뛴다(스냅샷의 35%가 0;
  data segment 끝 `Compiled::clean`까지는 복사). 같은 아카이브에서 task 준비 합 2.25 s → 1.10 s(0 건너뛰기) → 7 ms(view); 전체 Wasm 0.72 → 0.59–0.61 s.
- **[제안]** Linux는 arena를 wasm 메모리 위에 `MAP_FIXED`로 겹쳐 워커 쪽 복사도 없앤다(입력 `MAP_PRIVATE`, 전체 모듈의 출력 `MAP_SHARED`, 출력
  영역은 브로커가 `fallocate(PUNCH_HOLE)`로 0으로 만든다). 지금은 방향마다 브로커 쪽 memcpy 1회. macOS는 `mach_vm_remap`으로 가능할 수도 있다 *(확인 필요)*.
  코드에는 아직 없다.
- **[제안]** resource는 메시지로 보낸다(대개 작다 *(확인 필요: flate 모델 데이터의 크기)*).

### 4.2 컴파일과 워커 관리 [제안]

모두 코드에 없다.

- 컴파일 결과를 워커 사이에 공유한다(한 워커의 `Module::serialize`를 브로커가 다른 워커에 넘긴다; W개 워커의 컴파일 W배 → 1배, 디스크 캐시로
  확장 가능). 직렬화 산출물은 샌드박스 밖에서 `Module::deserialize`하면 안 된다(unsafe: 네이티브 코드 주입이 된다).
- 워커 수 상한과 놀고 있는 워커를 정리하는 timeout(지금은 동시에 `apply`를 부르는 스레드 수만큼 생겨 `Sandbox`가 살아 있는 동안 남는다).
- 워커마다 pool이 있어 W개면 스레드가 W × T개: 기본값을 호스트 pool 크기 / 동시 워커 수로 할지.
- 기동 비용을 숨기는 워커 미리 띄우기.

### 4.3 그 밖 [제안]

- 2.2의 Linux 메모리 한도.
- 워커의 stderr를 길이 제한을 두고 받아 실패 메시지에 붙인다(지금은 panic하면 종료 코드만 남는다; 단말에 그대로 내보내면 이스케이프 시퀀스 주입).
- 실행 시간 제한(in-process에도 없다: 동작을 바꾸지 않으려고 넣지 않았다. 필요하면 브로커가 벽시계나 CPU 시간으로 죽인다).
- 이 기계의 WSL은 홈이 drvfs라 그 아래 target에서 빌드가 깨진다: Linux 시험은 `CARGO_TARGET_DIR=/var/tmp/aqueous-target`.

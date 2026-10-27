# Aqueous 모듈 샌드박스 설계 초안

디코더가 아카이브의 Wasm 모듈을 실행할 때 Wasmtime 자체의 격리 말고 운영체제 수준의 격리를 하나 더 두는 방법을 다룬다. 지금까지 구현한 것과 측정한 것, 남은 일을 정리한다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다.

관련 코드:

- `aqr-decompress/src/sandbox/mod.rs`: 공개 API(`Sandbox`, `Sandboxed`, `worker_main`)와 브로커 쪽 워커 관리.
- `aqr-decompress/src/sandbox/proto.rs`: 브로커와 워커 사이의 메시지.
- `aqr-decompress/src/sandbox/worker.rs`: 워커의 초기화, 봉쇄, 요청 루프.
- `aqr-decompress/src/sandbox/os/`: OS별 `spawn`·`Child`·`lockdown`. `windows.rs`, `linux.rs`, `macos.rs`(Linux와 공유하는 기동과 종료 코드는 `unix.rs`), 그 밖의 시스템은 `unsupported.rs`.
- `aqr-decompress/src/bin/worker.rs`: 참고용 helper 바이너리 `aqr-decompress-worker`.
- `aqr-decompress/src/wasm.rs`: 모듈 실행(`load`, `Shared::apply`)과 워커 봉쇄 전의 `warm_up`.
- `aqr-decompress/tests/wasm.rs`: 모든 load/apply/decode를 샌드박스에서도 실행해 결과(실패 포함)가 같은지 확인한다.
- `aqr-decompress/tests/sandbox.rs`: 봉쇄(파일·프로세스·네트워크 접근 거부)와 기동 실패. macOS의 메모리 감시는 `os/macos.rs`의 단위 테스트가 확인한다.
- `aqr-decompress/examples/wasm.rs`: 네이티브, in-process Wasm, 샌드박스의 시간 비교.

---

## 0. 전제와 목표

- **[확정]** Wasmtime에서도 샌드박스 탈출 버그는 나올 수 있다. 아카이브가 담은 임의의 모듈(xform-design.md §5.7의 수준 2)을 실행하는 디코더는 이를 전제로 해야 한다. 그래서 모듈 실행을 OS가 봉쇄한 별도 프로세스로 옮겨, 탈출한 코드가 파일, 네트워크, 다른 프로세스에 닿지 못하게 한다.
- **[확정]** 라이브러리의 동작은 바꾸지 않는다. `Native`, `Wasm`, `decompress*`는 그대로 두고, 샌드박스는 호출자가 고르는 runner로만 제공한다. 샌드박스의 결과는 대응하는 runner의 결과와 같아야 하고, 실패도 같아야 한다. 예외는 워커를 띄우지 못했거나 워커가 예기치 않게 끝난 경우로, 이때는 resource 실패가 된다.
- **[확정]** 성능 손실을 최소화한다. 알려진 모듈(aqr-modules가 만든 것)은 지금처럼 네이티브로 돌리고 외부 모듈만 워커로 보낸다(`Sandboxed`). 그러면 흔한 경우의 비용은 0이다.
- **[확정]** 봉쇄할 수 없는 시스템에서는 워커를 띄우지 않는다. 봉쇄되지 않은 워커는 같은 사용자 권한으로 아무 일이나 할 수 있으므로, 프로세스를 나누는 것만으로는 아무것도 막지 못한다.

## 1. 구조

- **[확정]** 브로커와 워커로 나눈다. 브로커는 디코더 자신이고, 워커는 `Loaded::apply` 단위의 일을 받는 프로세스이다. 경계가 `Runner`/`Loaded`이므로 디코더의 나머지 코드는 바뀌지 않는다.
  > 모듈이 하는 일은 사실상 순수 함수 `(params, resources, inputs, sizes, limit) → outputs | Failure`이다. 호스트 함수도 메모리와 동기화만 다룬다. 따라서 워커에는 표준 입출력 말고는 어떤 자원도 필요 없고, OS 봉쇄를 가장 엄격하게 걸 수 있다.
- **[확정]** 워커 프로그램은 호스트가 정한다. `Sandbox::new(program, args)`로 helper 바이너리를 쓰거나, `Sandbox::current_exe(args)`로 자기 자신을 다시 실행한다. 어느 쪽이든 워커 쪽에서는 `sandbox::worker_main()`을 부른다. 참고용 helper로 `aqr-decompress-worker`(세 줄)를 둔다.
  > build.rs에서 helper를 직접 빌드하는 방식은 택하지 않았다. 바깥과 안쪽 cargo가 target 디렉터리를 공유하지 못해 wasmtime을 두 번 빌드하게 되고, 환경 전달과 vendoring이 번거롭다. 무엇보다 만든 바이너리를 런타임에 실행 가능한 위치에 둘 공식 수단이 없다. 임베드하면 Windows와 macOS에서 임시 파일 실행 문제가 생긴다. cargo의 artifact dependencies(`-Z bindeps`)가 안정화되면 다시 검토한다.
- **[확정]** 워커는 `Sandbox` 안에서 재사용된다. 놀고 있는 워커를 꺼내 쓰고, 없을 때만 새로 띄운다. 워커 하나는 받은 모듈들을 해시별로 들고 있다. 워커는 다른 `Sandbox`와 공유하지 않는다. 따라서 탈취된 워커가 영향을 줄 수 있는 것은 같은 `Sandbox`의 다음 결과뿐이다. 아카이브마다 `Sandbox`를 만들면 영향 범위가 그 아카이브로 한정된다.
- **[확정]** 워커는 받을 수 있는 것을 모두 받은 뒤에 스스로 봉쇄한다(`os::lockdown`). 받을 수 있는 것은 pool 스레드, 엔진, Cranelift 컴파일 스레드, 지연 로드되는 시스템 라이브러리이다. 봉쇄 전에 읽는 것은 HELLO와 스레드 수뿐이고, 아카이브에서 온 바이트는 하나도 읽지 않는다. Windows에서는 요청 루프가 메인 스레드에서 돈다. 초기화에 쓴 impersonation 토큰은 그 스레드에만 걸려 있다가 `RevertToSelf`로 내려놓인다. 모듈 실행은 스택이 큰 pool 스레드로 넘긴다.
- **[확정]** 메시지는 워커의 stdin/stdout으로 오간다. 브로커는 워커가 쓴 것을 신뢰하지 않는다. 길이는 할당 전에 상한과 대조하고(출력은 `sizes`, 메시지는 1 MiB), 출력 개수와 크기는 선언된 것과 정확히 같아야 한다. 워커가 위반하면 그 워커를 버리고 resource 실패로 처리한다.
- **[확정]** 입력과 출력을 합쳐 64 KiB 이상인 `apply`는 그것들을 워커별 공유 영역(arena)에 둔다. 메시지에는 길이만 싣는다. 그보다 작은 요청과 resource는 메시지로 보낸다.
  - arena는 브로커가 만들어 워커에 넘긴다(`os::arena`, `Child::share`, 워커의 `os::view`). 크기는 모자랄 때만 바꾼다. 2의 거듭제곱으로 올려(최소 1 MiB) 새로 만들어 넘기고, 이전 것은 양쪽에서 먼저 해제한다. 256 MiB를 넘는 arena는 그 요청 하나에만 쓰고 양쪽이 버린다. 큰 요청 뒤에 메모리를 붙잡아 두지 않기 위해서이다.
  - 배치는 메시지가 아니라 길이에서 정해진다(`proto::layout`). 입력들, 이어서 출력들을 차례로 놓고 각각 64 KiB에 맞춘다. 브로커와 워커가 같은 함수로 계산한다.
  - 워커는 arena의 입력을 그대로 읽고, 출력을 arena에 직접 쓴다(`wasm::Shared::apply_into`). in-process `apply`도 같은 경로로, 0으로 채운 `Vec`(calloc이라 쓸 때 0이 채워진다)에 쓴다. 파트 모듈의 `aqr_commit`은 바이트마다 push하던 것을 버퍼에 slice로 복사하게 됐다.
  - 브로커는 출력을 정확히 한 번만 읽어 자기 `Vec`으로 복사한다. 탈취된 워커는 다른 스레드로 arena를 계속 고쳐 쓸 수 있으므로, 같은 바이트를 두 번 읽는 검증(double fetch)을 하지 않는다. arena는 워커가 크기를 바꿀 수 없다(아래 OS별). 그래서 브로커가 매핑을 읽다가 SIGBUS를 맞을 일이 없다.
  - 선언된 출력 크기가 메모리 한도를 넘거나 arena를 만들 수 없으면 메시지로 보낸다. 어느 쪽이든 결과는 in-process와 같다. 터무니없는 크기는 무엇을 만들기 전에 메모리 한도 실패가 된다(`Shared::check`).
  - 남는 추가 복사(in-process 대비)는 방향마다 브로커 쪽 memcpy 1회이다. 전에는 방향마다 파이프 복사 2회에 syscall이 붙었다.
- **[확정]** 워커의 환경 변수는 비우고(Windows는 `SystemRoot`만 남김), stderr는 버리고, 현재 디렉터리는 시스템 루트로 둔다.

## 2. OS별 봉쇄

### 2.1 Windows

- **[확정]** Chromium의 렌더러와 같은 방식으로 봉쇄한다.
  - 토큰: 사용자의 모든 SID를 deny-only로 하고, 특권은 없앤다. restricting SID는 NULL SID로 두어 이름으로 아무것도 열지 못하게 하고, integrity는 low로 한다.
  - job: 자식 프로세스 금지(`ACTIVE_PROCESS` 1), UI 제한 전부, job 핸들이 닫히면 종료, 처리되지 않은 예외에서 종료, 요청마다 정하는 메모리 한도.
  - mitigation: Win32k system call 금지, strict handle check, extension point 금지, 폰트 금지, 원격·low-label 이미지 금지, system32 우선, 자식 프로세스 제한.
  - 상속 핸들: `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 stdin/stdout 두 개만. 콘솔 없음(`DETACHED_PROCESS`).
- **[확정]** 워커는 사용자와 접근 권한이 같은 restricted 토큰(사용자의 모든 SID와 RESTRICTED를 restricting SID로 둔 것, low integrity)을 impersonate한 상태로 시작한다. 이 토큰으로 시스템 라이브러리를 로드하고, 초기화가 끝나면 `RevertToSelf`로 내려놓는다. 프로세스 토큰이 restricted가 아니면, 즉 샌드박스 밖에서 실행됐으면 워커는 서비스를 거부한다.
  > 초기 토큰이 restricted가 아니면 커널이 impersonation을 거부한다(`STATUS_BAD_IMPERSONATION_LEVEL`, 0xC00000A5). restricted 프로세스는 restricted 토큰만 impersonate할 수 있다.
- **[확정]** 메모리 한도는 job의 프로세스 메모리 한도(commit 기준)로 건다. 값은 요청의 메모리 한도에 전송 바이트와 여유분 1 GiB를 더한 것이다. wasmtime의 가상 주소 예약은 commit이 아니므로 이 한도와 부딪히지 않는다.
- **[잠정]** arena는 pagefile 기반의 이름 없는 section이다. 브로커가 `DuplicateHandle`로 워커 프로세스에 `FILE_MAP_READ | FILE_MAP_WRITE` 핸들을 만들고, 그 값을 메시지로 알린다. 워커는 매핑한 뒤 핸들을 닫는다. section은 크기를 바꿀 수 없다. 크로스 컴파일(windows-gnu)과 clippy만 확인했고 Windows에서 실행해 보지는 않았다.
- **[확정]** ACG(동적 코드 금지)는 걸 수 없다. Cranelift가 프로세스 안에서 코드를 실행 가능하게 만들기 때문이다. 걸려면 컴파일을 다른 프로세스로 빼야 하는데(Edge의 out-of-process JIT), 비용에 비해 얻는 것이 적다.
- **[제안]** CET 강제 모드는 확인하지 않았다. wasmtime은 트랩에서 복귀할 때 컨텍스트의 RIP를 바꾸므로 shadow stack과 충돌할 수 있다 *(확인 필요)*.
- **[제안]** 초기화 뒤에 integrity를 untrusted로 더 내리는 것(Chromium의 delayed integrity)은 아직 하지 않았다.

### 2.2 Linux

- **[확정]** 워커는 스스로 봉쇄한다. `PR_SET_NO_NEW_PRIVS`를 걸고, fd 4 이상을 전부 닫은 뒤(`close_range`), seccomp 필터를 `SECCOMP_FILTER_FLAG_TSYNC`로 모든 스레드에 건다. fd 3은 arena의 fd가 오는 소켓이다.
  - 허용 목록: 메모리(`mmap`, `munmap`, `mprotect`, `madvise`, `mremap`, `brk`), 동기화(`futex`, `sched_yield`), `CLONE_THREAD`가 있는 `clone`, 시그널(wasmtime의 트랩 처리용), 시간, `getrandom`, 자기 프로세스로의 `tgkill`, `PR_SET_NAME`만 허용하는 `prctl`, 읽기만 허용하는 `prlimit64`, fd 3(arena 소켓)에서만 허용하는 `recvmsg`, 그리고 이미 열린 fd에 대한 `read`·`write`·`fstat`·`close`.
  - `clone3`는 인자가 구조체라 필터링할 수 없으므로 `ENOSYS`를 돌려줘 C 라이브러리가 `clone`으로 폴백하게 한다.
  - 그 밖의 호출은 `EPERM`을 돌려주고, 아키텍처가 다르거나 x32 호출이면 프로세스를 죽인다.
- **[확정]** arena는 memfd이다. 브로커가 크기를 정한 뒤 `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`로 봉인한다. 워커에는 fd 3의 socketpair로 `SCM_RIGHTS`를 써서 보낸다. 워커는 seccomp 때문에 어차피 `ftruncate`를 부를 수 없다. Linux(aarch64, Lima)에서 `tests/wasm.rs`가 arena로 통과했다. 필터에서 `recvmsg`를 빼면 워커가 arena를 받지 못해 실패하는 것도 확인했다.
- **[잠정]** OS 수준의 메모리 한도는 아직 없다. 워커 안의 `ResourceLimiter`는 그대로 동작한다. 따라서 남는 위험은 탈취된 경우의 메모리 고갈(DoS)이다.
  > rlimit(`RLIMIT_DATA`)은 스레드 스택과 wasmtime이 실제로 쓰는 부분까지 세므로 여유분을 정하기 어렵다. cgroup v2의 `memory.max`가 맞지만, delegation이 늘 되어 있지는 않다.
- **[제안]** 심층 방어로 user/pid/net/mount 네임스페이스나 Landlock을 더할 수 있다. 비특권 user ns는 배포판에 따라 막혀 있으므로(Ubuntu 24.04+), 없는 환경에서도 seccomp만으로 동작해야 한다.
- **[제안]** 부모가 죽은 뒤 워커를 정리하는 일은 파이프 EOF에 맡긴다. `PR_SET_PDEATHSIG`는 프로세스가 아니라 워커를 띄운 스레드가 끝날 때 신호를 보내므로, 스레드가 오가는 호스트에서는 워커가 엉뚱하게 죽는다.

### 2.3 macOS

- **[확정]** 워커는 스스로 봉쇄한다. fd 4 이상을 전부 닫고(`proc_pidinfo(PROC_PIDLISTFDS)`로 열린 것을 나열한다. fd 3은 arena 소켓이다), Seatbelt 프로필 `(version 1) (deny default)`를 `sandbox_init_with_parameters`로 건다. 문서상 deprecated이고 헤더에도 없지만(`sandbox_init`은 이름 있는 프로필만 받는다), Chromium과 Firefox가 지금도 쓴다.
  - 허용하는 것이 하나도 없다. 프로필은 시스템이 검사하는 연산(파일, 소켓, 프로세스, Mach 서비스 lookup, sysctl 등)만 막고, 메모리, 스레드, 시그널, 자기 task의 Mach 포트, 이미 열린 fd는 검사 대상이 아니기 때문이다. Linux의 seccomp처럼 시스템 호출 단위로 걸러야 할 것이 없다.
  - 위반은 sandboxd가 시스템 로그에 남긴다. 탈출한 코드의 흔적이 되므로 `(with no-log)`는 쓰지 않는다.
- **[확정]** wasmtime의 기본 트랩 처리(Mach exception port)는 샌드박스 아래에서도 동작한다. `unreachable`과 32비트 메모리의 guard page 접근(`EXC_BAD_ACCESS`)이 모두 in-process와 같은 실패가 된다(`tests/wasm.rs`의 `whole`). 따라서 `Config::macos_use_mach_ports(false)`는 필요 없다. Cranelift의 코드도 그대로 실행 가능해진다. 서명하지 않았거나 ad-hoc으로 서명한 실행 파일에서 확인했다.
- **[확정]** 메모리 한도는 브로커가 건다. 워커마다 감시 스레드를 두고, 10 ms마다 `proc_pid_rusage`의 `ri_phys_footprint`(시스템이 그 프로세스의 메모리로 세는 양)를 보다가 한도를 넘으면 죽인다. 한도의 값은 Windows와 같다(요청의 메모리 한도 + 전송 바이트 + 여유분 1 GiB). 감시 스레드는 `Child`를 약한 참조로만 들고 있어서 `Child`가 버려지면 끝난다.
  > kill은 pid가 아니라 `std::process::Child`를 통해 보낸다. 이미 wait한 자식에게는 std가 신호를 보내지 않으므로, pid가 재사용돼도 엉뚱한 프로세스를 죽이지 않는다. footprint는 재사용된 pid의 것을 읽을 수 있지만 읽기만 하므로 해가 없다.
  > 폴링이므로 10 ms 사이에 한도를 크게 넘을 수는 있다. 막으려는 것은 탈취된 워커가 메모리를 고갈시키는 DoS이므로 이 정도로 충분하다. 커널이 강제하는 수단(`task_set_phys_footprint_limit`, jetsam 한도)은 비공개이거나 특권이 필요하다.
- **[확정]** arena는 POSIX 공유 메모리이다. `shm_open`으로 만들자마자 `shm_unlink`하고, fd를 Linux와 같이 소켓으로 보낸다. macOS의 공유 메모리는 `ftruncate`를 한 번만 받는다. 그래서 워커가 크기를 바꿀 수 없고, 키울 때는 새로 만든다. 이 제약 때문에 Linux도 키울 때 새로 만드는 방식으로 맞췄다. Seatbelt 아래에서도 받은 fd의 `recvmsg`와 `mmap`은 막히지 않는다.
- **[제안]** hardened runtime으로 서명한 실행 파일에서는 JIT를 위한 entitlement(`com.apple.security.cs.allow-jit`, 또는 wasmtime이 `MAP_JIT`를 쓰지 않는다면 `allow-unsigned-executable-memory`)가 필요할 것이다 *(확인 필요)*. 배포 형태가 정해질 때 확인한다.

### 2.4 그 밖의 시스템

- **[확정]** `os/unsupported.rs`가 워커 시작을 거부한다(`io::ErrorKind::Unsupported`). 그래서 `Sandbox`는 resource 실패를 낸다.

## 3. 검증과 측정

- **[확정]** 동치성은 `tests/wasm.rs`로 확인한다. 기존 테스트 11개의 모든 load/apply/decode를 샌드박스에서도 실행해 결과와 실패 메시지가 같은지 본다. tasks, channels, deadlock, 메모리 한도, 프로파일 위반, 손상된 아카이브가 포함된다. Windows, Linux(WSL2의 x86_64와 Lima의 aarch64), macOS 모두 통과했다.
- **[확정]** 봉쇄는 `tests/sandbox.rs`의 `locked_down`으로 확인한다. 워커에게 파일 읽기, 파일 쓰기, 프로세스 생성, loopback 연결을 시킨다(`Sandbox::probe`).
  - Windows에서는 앞의 셋이 ACCESS_DENIED이고, 소켓은 `WSAStartup`부터 실패한다(std가 이를 panic으로 처리한다).
  - Linux(x86_64, aarch64)와 macOS에서는 넷 다 `EPERM`이다.
  - `RevertToSelf`, seccomp, Seatbelt를 빼면 이 테스트가 실패하는 것도 확인했다. macOS의 메모리 감시 테스트(`memory_limited`)도 kill을 빼면 실패한다.
  > 처음에는 요청 루프가 pool 스레드에서 돌아서, `RevertToSelf`를 빼도 테스트가 통과했다. impersonation이 걸린 메인 스레드를 아무도 건드리지 않았기 때문이다. 탈출한 코드가 메인 스레드를 가로채면 그 토큰을 쓸 수 있으므로, 요청 루프를 메인 스레드로 옮겼다.
- **[확정]** 비용 항목. release, 2026-10에 Windows와 macOS(Apple Silicon)에서 측정했다. Linux는 재지 않았다. macOS의 모듈 컴파일은 따로 재지 않았다.

  | 항목 | Windows | macOS | 단위 |
  |---|---|---|---|
  | 워커 기동(spawn, 봉쇄, 워밍업) | 약 7 ms. 실행 파일을 처음 띄울 때만 약 160 ms | 약 7 ms. 처음만 약 140 ms | 워커마다 |
  | 모듈 컴파일 | flate 84 ms, jpeg 35 ms, mp3 16 ms | | 워커 × 모듈마다 |
  | `apply` 왕복 | 약 36 µs 추가(57 µs, in-process는 21 µs) | 약 11 µs 추가(39 µs, in-process는 28 µs) | `apply`마다 |
  | 바이트 전달(파이프) | 입력 1 MiB와 출력 1 MiB를 합쳐 약 0.4 ms | 약 0.19 ms 추가(0.35 ms, in-process는 0.16 ms) | 입력 + 출력 바이트 |
  | 바이트 전달(arena) | | 1 MiB + 1 MiB에 약 0.05 ms 추가(0.22 ms, in-process는 0.17 ms). 16 MiB + 16 MiB에 0.7 ms, 64 MiB + 64 MiB에 2.5 ms 추가 | 입력 + 출력 바이트 |

  > 64 MB 텍스트를 gzip한 아카이브(출력 17.8 MB)를 `Sandbox`로 디코딩하면 0.78–0.94초가 걸렸다. in-process Wasm은 0.76–0.78초였다. 차이의 대부분은 고정비(기동과 flate 컴파일, 약 90 ms)이고, 바이트 전달은 약 5 ms이다.
  > macOS에서 저장소의 텍스트로 만든 같은 크기의 아카이브(출력 18.8 MB)는 `Sandbox`로 1.10–1.19초, in-process Wasm으로 1.00–1.05초였다. 새로 빌드한 실행 파일을 처음 띄울 때는 1.72초였다. 시스템이 처음 실행되는 바이너리를 검사하는 비용으로 보인다 *(확인 필요)*.
  > arena를 둔 뒤 macOS에서 같은 아카이브는 `Sandbox`로 0.64–0.67초, in-process Wasm으로 0.62–0.64초였다. 앞의 측정보다 전체가 빠른 것은 기계가 한가했기 때문이다.
- **[확정]** 바이트당 비용은 모듈의 처리 속도에 비례해 커진다. 파이프로 보낼 때는 Wasm 처리 속도가 R MB/s인 해제 모듈이라면 비율이 대략 `0.25 × R / 1000`이었다. 20–30 MB/s에서 1% 미만이고, 500 MB/s에서 약 12%, 1–2 GB/s에서 25–50%이다. arena로 보내면 macOS에서 MiB(입력 + 출력)당 0.30 ms이던 것이 0.04 ms가 되었다. 1 GB/s 모듈로 쳐도 4% 안팎이다.

## 4. 다음에 할 일

### 4.1 바이트당 비용 제거

- **[확정]** 공유 영역(arena)과 `apply_into`는 구현했다(§1, §2). 남은 것은 아래이다.
- **[제안]** Linux에서는 arena를 wasm 메모리 위에 겹쳐 매핑해 워커 쪽 복사도 없앤다. 입력은 `MAP_PRIVATE|MAP_FIXED`로, 전체 모듈의 출력은 `MAP_SHARED|MAP_FIXED`로 매핑한다. 전체 모듈의 출력 영역은 요청 전에 브로커가 `fallocate(PUNCH_HOLE)`로 0으로 만든다. 새 wasm 메모리가 0이어야 하기 때문이다. 배치는 이미 64 KiB에 맞춰져 있다. 이것은 xform-design.md에 적어 둔 task 스냅샷의 CoW 방안과 같은 기법이다.
  > 그러면 추가 복사(in-process 대비)가 Linux에서 0회가 된다. 지금은 방향마다 1회이다. Windows는 wasmtime이 메모리를 placeholder 없이 예약하고, `MemoryCreator`가 호스트가 만드는 메모리에는 쓰이지 않으므로 매핑을 바꿀 수 없다. macOS는 `mach_vm_remap`으로 가능할 수도 있다 *(확인 필요)*.
- **[제안]** Windows에서 `tests/wasm.rs`(특히 `large`)를 돌리고 비용을 다시 잰다.
- **[제안]** resource(`Resource`, `Arc<[u8]>`)는 지금처럼 메시지로 보낸다. 크기가 대개 작기 때문이다 *(확인 필요: flate 모델 데이터의 크기)*.

### 4.2 컴파일과 워커 관리

- **[제안]** 컴파일 결과를 워커 사이에 공유한다. 한 워커가 컴파일한 결과(`Module::serialize`)를 브로커가 받아 다른 워커에 그대로 넘긴다. deserialize는 샌드박스 안에서만 일어나므로 안전하다. 동시 워커 W개의 컴파일 비용이 W배에서 1배로 줄고, 디스크 캐시로도 확장할 수 있다.
  > 직렬화 산출물은 절대 비샌드박스 프로세스에서 `Module::deserialize`하면 안 된다. `deserialize`는 unsafe라서, 그렇게 하면 그대로 네이티브 코드 주입이 된다.
- **[제안]** 워커 수에 상한을 두고, 놀고 있는 워커를 정리하는 timeout을 둔다. 지금은 동시에 `apply`를 부르는 스레드 수만큼 워커가 생기고, `Sandbox`가 살아 있는 동안 계속 남는다.
- **[제안]** 워커마다 pool을 따로 가지므로 동시 워커 W개면 스레드가 W × T개가 된다. 기본값을 호스트 pool 크기 대신 호스트 pool 크기 / 동시 워커 수 정도로 정할지 검토한다.
- **[제안]** 기동 비용을 숨기기 위해 워커를 미리 띄우는 것을 검토한다.

### 4.3 그 밖

- **[제안]** aqr CLI에 연결한다. 숨은 워커 인자로 자기 자신을 다시 실행하게 하고, `--sandbox`(또는 기본값)로 외부 모듈을 샌드박스에서 돌린다. 기본값으로 할지는 정해야 한다.
- **[제안]** 2.2의 Linux 메모리 한도.
- **[제안]** 워커의 stderr를 길이 제한을 두고 브로커가 받아 실패 메시지에 붙인다. 지금은 워커가 panic하면 종료 코드만 남는다. 단말에 그대로 내보내면 이스케이프 시퀀스 주입이 가능하므로 그렇게 하지 않는다.
- **[제안]** 실행 시간 제한. in-process에도 없으므로 동작을 바꾸지 않으려고 넣지 않았다. 필요하면 브로커에서 벽시계나 CPU 시간 기준으로 워커를 죽이는 방식이 된다.
- **[제안]** 이 기계의 WSL은 홈 디렉터리가 drvfs(`/mnt/d`)라 그 아래 target에서는 빌드가 깨진다. Linux 테스트는 `CARGO_TARGET_DIR=/var/tmp/aqueous-target`으로 돌린다.

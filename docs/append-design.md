# 덧붙이기(`-a`)와 중단

증분 백업에는 아카이브에 segment를 덧붙이는 `-a`가 필요하다. 이 설계는 `-c`를 "빈 아카이브를 만들고
`-a`를 돌리는 것"으로 본다. `-a`는 덧붙이기만 하는 트랜잭션이다. 덧붙이는 것 말고 고치는 것은 옛
footer의 2바이트뿐이고, commit은 언제든 할 수 있다. 그러면 어디서 멈추든 마지막 commit까지 담은 올바른
아카이브가 남고, 같은 `-a`로 이어 쓸 수 있다. 이 성질은 footer 재구성(spec의 "Repair")에 기대지 않는다.

정한 것:

- commit은 옛 footer를 2바이트 spacer header로 죽이는 것이다(아래 "footer를 옮기는 법"과 그 전제).
- 멈추면 commit하는 것이 기본이고, 전부 아니면 전무인 `--atomic`을 옵트인으로 둔다.
- `-a`는 갱신(없거나 바뀐 것만)이고 아무것도 지우지 않는다. tombstone은 동기화 verb `-s`(`--sync`)만 쓴다.
- 암호화된 아카이브에는 덧붙이지 않는다(nonce 재사용).
- `last-segment-intended`(-3)를 `segment-status`로 바꿔 "미완성"도 함께 나타낸다.
- internal import는 이 작업 밖의 추가 작업으로 남긴다.

아카이브 구조는 spec의 "Unencrypted archive", "Locating an archive", "Interpreting an archive"가
정하고, segment를 닫는 정책과 잘림(E4)에 대한 논의는 `recovery-design.md` §4–5에 있다.

## 구현 (2026-11-05)

정한 대로 구현했다. 커밋은 spec, aqr-formula, aqr-compress, aqr 순서이며, 각각 시험이 붙어 있다.

- spec: "Appending to an archive", "trim a located archive", `segment-status`.
- aqr-formula: `locate`·`trim`·`footer_of`·`block_at`, `Status`, `SegmentWriter::{footer, end, appending,
  replacing, pad}`, `kill`·`crossing`, tombstone(`EntryNode::Special(0)`) 쓰기.
- aqr-compress: `Output`(commit하는 출력), `Stop`, `Appending`, `Output::commits_once`. 배치를 만드는 일은
  writer가 아닌 스레드에서 돌고, writer는 그동안 stop을 본다.
- aqr: `-c`의 commit과 두 단계 중단(`stop.rs`), `--atomic`, `-a`·`-s`(`append.rs`), `--ipc`의
  `finish`, 위치를 찾아 읽기(앞의 stub, 뒤의 꼬리), 미완성 표시, 중단된 해제의 정리.

남은 것:

- 프론트엔드(aqr-win, aqr-mac)의 UI. `finish`를 보낼 버튼이나 대화상자가 아직 없다. 프로토콜과
  `aqr_ipc::Ipc::finish`만 있다.
- `--rm`과 중단. 지금은 멈추면 입력을 하나도 지우지 않는다. commit마다 지우는 것은 아직 하지 않았다.
- 다른 형식의 해제(aqr-unpack)는 아직 멈추면 이전처럼 끝난다. 큰 content를 나눠 쓰면서 그 안에서
  멈추는 것도 하지 않았다.
- 해제의 정리는 다 쓰지 못한 fork가 있는 파일을 남긴다. 파일 자체는 온전하다.
- Windows에서 파일 잠금(`File::try_lock`)과 콘솔 닫힘의 처리는 type check만 했고 실행해 보지 않았다.
- 10gb에서 commit의 비용과 멈출 때의 지연은 Mac에서 재야 한다. 이 머신에서 16 MB짜리 시험
  디렉터리를 `-T 1 -B 100K -S 1`로 만들면 SIGTERM부터 끝까지 50 ms 안에 끝났다.
- internal import(추가 작업). 암호화한 증분 백업(별도 설계).

## 설계 전 (2026-11-04)

- **압축.** 아카이브는 `OUTPUT.PID.part`로 쓰고 다 쓴 뒤 rename한다(`write_whole`, `main.rs`).
  잡을 수 있는 중단(SIGINT·SIGTERM·SIGHUP, Windows의 Ctrl-C·Ctrl-Break·콘솔 닫힘, `--ipc`의
  `cancel`)에서는 `stop.rs`의 handler가 `.part`를 지운다. 얼마나 썼든 아무것도 남지 않는다.
- **잡을 수 없는 중단**(SIGKILL, 정전)에서는 `.part`가 남는다. 닫힌 segment까지는 footer 재구성으로
  읽힌다. 다만 `last-segment-intended`가 없으므로 뒤를 잃었을 수 있다고 알리고 실패한다.
- **단일 파일 압축·해제**도 같은 `.part` 방식이다.
- **디렉터리 해제에는 보호가 없다.** 내용이 아직 쓰이지 않은 파일은 빈 파일로, 쓰던 파일은 잘린
  파일로 남고, metadata와 symlink는 하나도 적용되지 않는다(아래 "해제").
- `-a`는 없다. spec은 처음부터 덧붙이기를 염두에 두었다(Introduction: "extended by appending a
  segment without rewriting what precedes it"). segment는 self-contained이고("Content blocks"),
  뒤 segment가 앞의 엔트리를 더하고 바꾸고 지운다("Merging segments").

## `-c` ≡ 빈 아카이브 + `-a`

구조로는 이미 거의 그렇다. 지금의 `-c`(`compress_tree_noting`)는 self-contained segment를 차례로 쓰고,
segment 사이에 걸친 것은 batch의 순서(계획)뿐이다. `-a`도 자기 입력으로 같은 계획을 세운다. 차이는
시작 상태다. `-a`는 기존 아카이브의 끝 오프셋, 합친 트리, segment 수, 이미 쓴 archive-wide link group
번호에서 시작하고, `-c`는 header에서 시작한다. 그러니 엔진(aqr-compress)이 "앞선 상태"를 받게 하면
`-c`는 그 상태가 빈 경우다.

spec에서 "빈 아카이브"(header와 footer뿐)는 metadata block이 없으니 **빈 파일 하나**로 풀린다. 빈
디렉터리는 루트만 있는 metadata block이 있어야 한다. `-c`가 그런 빈 commit을 따로 쓸 필요는 없다.
첫 commit 전까지는 아무것도 보이지 않으면 된다(아래 "출력 이름").

## footer를 옮기는 법

덧붙이려면 footer를 뒤로 옮겨야 한다. **두 footer가 잠시 공존하게 두고, 옛 footer를 2바이트로 죽이는
것을 commit으로 삼는다.** 원자적인 큰 쓰기는 필요 없고 순서만 맞으면 된다.

순서는 spec의 "Appending to an archive"(commit)가 정한다: 옛 footer F_k 뒤에 segment의 블록, metadata
block M_{k+1}, 새 footer F_{k+1}을 쓰고 sync한 뒤, F_k의 첫 2바이트를 spacer header로 덮어 sync하고(commit),
나머지를 0으로 채운다. F_{k+1}을 쓰는 동안은 처음 만나는 footer가 F_k이므로(spec, "trim a located archive")
읽는 사람에게는 옛 상태가 보인다.

### 전제와 엣지 케이스

commit은 쓰기 한 번이 찢어지지 않는다는 데 기댄다. 그러니 다음을 전제로 둔다.

**섹터.** 장치는 512바이트(또는 그 배수) 섹터를 원자적으로 쓴다(NVMe AWUPF ≥ 1 LBA, CoW 파일 시스템은
블록 단위). 길이 h인 header를 **파일** 오프셋 p에 쓰는 일은 `p % 512 ≤ 512 − h`일 때 원자적이다. 512의
배수인 섹터 크기 S에 대해서도 같은 조건이 성립한다(`p % S`가 `S − h`를 넘으면 `p % 512`도 넘는다).
네트워크 파일 시스템(NFS, SMB)의 sync 의미는 서버에 달려 있으므로 보장 밖이다.

**정렬은 파일 기준이다.** 블록이 짝수 오프셋에 오는 것은 아카이브의 시작을 기준으로 한 것이다. 실행 파일
뒤에 붙인 아카이브(SFX)는 시작이 홀수일 수 있고, 다 쓴 아카이브에 나중에 stub을 앞에 붙일 수도 있다.
그러니 정렬을 spec이 보장하게 할 수는 없고, 덧붙이는 쪽이 그때의 파일 오프셋으로 판단한다.

- **새 footer는 writer가 정렬한다.** commit마다 쓰는 F_{k+1}의 파일 오프셋이 조건을 어기면, M_{k+1} 뒤에
  2바이트 spacer(`00 00`)를 하나 넣어 옮긴다. 2바이트를 옮기면 `p % 512`가 511(또는 510)에서 벗어난다.
  비용은 1/256의 확률로 2바이트다. 그래서 Locate의 확인은 metadata block 뒤의 spacer block을 건너뛰고 본다.
- **이미 있는 footer는 맞을 수도 있고 아닐 수도 있다.** aqr가 쓰지 않았거나 나중에 위치가 바뀌어 조건을
  어기는 footer(512분의 1 남짓)는 2바이트로 죽일 수 없다. 이 경우를 쪼개서 넘길 방법은 없다. header는
  footer의 첫 바이트에 와야 하고, 찢어진 상태(`00 5B…`, `90 xx…`)는 엉뚱한 블록 경계나 모르는 critical
  type이 된다. 이때의 첫 commit만 예전 방식(footer를 잘라 내고 이어 쓰기)으로 하고, 그 사이의 중단은
  footer 재구성에 맡긴다. 그 뒤의 footer는 위의 정렬을 따르므로 이 경우는 아카이브마다 한 번 생길 수
  있을 뿐이다. `-v`로 알린다.

**footer의 길이.** 죽이는 header의 길이는 spec이 정한다(F_k가 257바이트 이하면 2바이트). aqr의 footer는 30바이트
이하라 늘 2바이트다.

**밀어내는 크기에는 하한이 필요 없다.** 이 방식은 footer를 제자리에서 옮기지 않는다. 새 블록, M_{k+1},
F_{k+1}은 모두 F_k(와 패딩)의 **끝 뒤에** 쓰므로 옛 footer와 겹치는 일이 없다. 새 segment가 tombstone 하나뿐인
metadata block이어도 마찬가지다. 크기가 문제가 되는 것은 아래의 예약 방식처럼 F_k보다 앞이나 F_k와 겹치는
자리에 무언가를 쓸 때다. 그 방식을 넣으려면 거리가 F_k의 길이 이상이어야 한다는 조건이 붙는다.

**거절하는 경우.**

- 아카이브 뒤에 우리 것이 아닌 바이트가 있을 때(서명, 전송이 붙인 것). 우리 것은 완결되었거나 쓰다 만
  commit이다.
- 암호화된 아카이브(아래). 이 경우에는 `-a`를 허용하지 않는다.
- size hint block이 있는 아카이브. 제자리에서 고칠 수 없다. aqr는 암호화할 때만 이것을 쓴다.

overlay 아카이브의 footer(`OVERLAY_FOOTER_TYPE`)도 똑같이 죽인다.

순서가 지켜지면 각 쓰기 사이에서 죽었을 때 남는 것은 다음과 같다.

| 어디서 죽었나 | 남는 것 |
| --- | --- |
| 블록이나 M_{k+1}을 쓰는 중 | 옛 상태와 꼬리 |
| F_{k+1}이 찢어졌거나 0으로 채워졌을 때 | 옛 상태와 꼬리. F_k가 처음 footer다 |
| F_{k+1}을 다 쓴 뒤, commit 전 | 옛 상태. 꼬리는 완결된 commit 하나다 |
| commit 뒤 | 새 상태 |

다음 `-a`는 꼬리를 보고 판단한다. 꼬리가 완결된 commit(F_{k+1}까지 맞음)이면 F_k를 죽여 앞으로 진행하고,
아니면 잘라 낸다.

### 고려하고 버린 것

- **footer를 잘라 내고 이어 쓰기.** 새 footer를 쓰기 전까지 footer가 없어 재구성에 기대야 한다. 정렬이 맞지
  않는 옛 footer에 대한 첫 commit에만 쓴다(위).
- **중간의 footer를 영구히 건너뛰게 하는 spec 변경.** commit한 아카이브가 지금의 디코더에서 읽히지 않는다.
- **새 footer를 멀리 예약해 두고 그 사이의 spacer 안에 쓴 뒤 spacer의 header를 바꾸기.** header를 바꾸는
  순간 앞에서부터 읽으면 M_{k+1}이 보이는데 끝의 footer는 M_k를 가리킨다(footer를 먼저 고치면 그 반대).
  두 자리를 고쳐야 하므로 한 번의 쓰기로 맞출 수 없다. 살아 있는 옛 footer가 이미 그 뒤를 가려 주므로
  숨길 필요도 없다. 예약의 이점은 쓰는 동안에도 파일 끝이 올바른 footer라 읽는 쪽이 빠른 경로를 탄다는
  것 하나다. 필요하면 형식은 그대로 두고 writer만 바꿔 얻을 수 있다. 끝 footer를 멀리 써 두고 commit 때
  M_{k+1}을 가리키게 고친 뒤, M_{k+1} 바로 뒤에 guard footer를 두고, F_k를 죽인다. 실행이 끝나면 guard
  뒤를 잘라 낸다. 이때는 그 거리가 F_k의 길이 이상이어야 한다. 처음 구현에는 넣지 않는다.

## 명세 변경

최종 상태는 지금의 spec 그대로이므로, 바뀌는 것은 잠시 공존하는 상태를 어떻게 읽을지 하나다.

- **Locate.** spec의 "trim a located archive". 처음에는 마지막 두 metadata block만 보려 했으나, 한 commit이
  여러 segment일 수 있어서(`--atomic`의 `-a`) 첫 metadata block까지 거슬러 올라간다.
- **`segment-status`.** 아래 "표식".
- **compressor의 의무.** 아카이브에 footer는 끝에 하나뿐이다(지금과 같다). 두 footer가 공존하는 것은 쓰는
  도중에만 생기는 상태이고, 덧붙이는 쪽은 그것을 남기지 않는다. 남은 경우 다음에 덧붙이는 쪽이 먼저
  정리한다. 앞의 footer는 metadata block 바로 뒤에 쓴다.
- **Repair.** 바뀌지 않는다. commit을 마친 아카이브의 옛 footer는 spacer이므로, 잘림(E4)에는 지금처럼 footer
  재구성이 필요하다. footer 재구성에 기대지 않는 것은 쓰는 도중의 중단에 한한다.
- **호환성.** commit을 마친 아카이브는 지금의 디코더에서도 읽힌다. 지금의 디코더가 공존 상태(쓰는 도중이거나
  commit 직전에 죽은 파일)를 읽으면 잘못 읽는 대신 실패한다.

## commit 규약

1. `-a`는 아카이브를 연다. advisory lock(`flock`, `LockFileEx`)으로 쓰는 쪽을 하나로 묶는다. 끝을 찾고,
   꼬리가 있으면 위의 표대로 앞으로 진행하거나 잘라 낸다.
2. 블록과 metadata block을 쓰고 sync, footer를 쓰고 sync, 옛 footer를 죽이고 sync, 남은 바이트를 0으로
   채운다. macOS의 `fsync`는 순서를 보장하지 않으므로 중간 sync에는 `F_BARRIERFSYNC`를, commit에는
   `F_FULLFSYNC`를 쓴다.
3. commit 하나의 비용은 footer 20바이트 남짓, 패딩 1바이트 이하, sync 셋이다. 10gb에서 segment가 7개이므로
   무시할 만하다.

## commit 지점

- **주기적 commit**은 segment를 닫을 때 한다(`-S`, 기본 256M; 암호화하면 하지 않는다). 잡을 수 없는 중단에서 잃는 것은 이
  단위다. `-S`를 줄이면 segment마다 모듈과 사전을 다시 싣고 segment를 넘는 중복 제거도 끊긴다
  (`recovery-design.md` §5: 64M에서 비용이 세 배). 그래서 크기 대신 시간(예: 10분)으로 닫는 선택지를
  나중에 고려한다.
- **멈출 때의 commit**은 열린 segment를 그때까지 쓴 batch로 닫는다. 다 쓴 batch는 블록이 온전하고, 빠진
  것은 metadata block과 footer뿐이다. 중단되지 않으면 비용이 0이고, 잃는 것은 진행 중이던 batch(`AHEAD`
  = 2, 입력 1 GiB 남짓)뿐이다. 세부는 다음과 같다.
  1. 첫 신호에서 handler는 atomic flag만 세운다. writer는 이미 `LOOK`(10 ms)마다 깨어 있다.
  2. `Writer::put`이 writer 스레드에서 직접 돌리는 `finish(i, &self.w, …)`(인코딩, 검증)를 밖으로 옮겨,
     writer가 계산 중인 batch를 기다리지 않게 한다. `SegmentWriter::finish`·`finish_mirrored`는
     `&self`이므로 계산 중인 작업과 함께 빌릴 수 있다.
  3. commit되는 것은 `w.commit`과 `write_all`을 마친 batch의 content다. `same_as`인 content는 대상도
     commit됐을 때만 넣는다. `self.segment`는 지금 `write` 첫머리에서 늘리는데, commit할 때 늘리도록
     옮긴다. 디렉터리, symlink, 특수 파일은 모두 넣는다.
  4. 쓰는 순서: 붙잡아 둔 batch(mirror 모드), metadata의 mirror, metadata block(스레드 1로 인코딩한다.
     pool은 버려진 batch로 바쁘다), sync, footer. 출력의 flush는 aqr-compress가 할 수 있어야 하므로,
     `Write`에 `end()`를 더한 출력 trait을 받는다.
  5. 분석 중인 스레드는 join하지 않는다. main이 준 callback이 요약과 `--stats`를 쓰고 130으로 `exit`한다.
     `stop::now`가 이미 이렇게 한다.

## 중단의 의미

| 무엇이 왔나 | 기본 | `--atomic` |
| --- | --- | --- |
| 첫 Ctrl-C, SIGTERM, SIGHUP, ipc `finish` | 열린 segment를 닫아 commit하고 exit 130 | 시작 전 상태로 되돌린다 |
| 두 번째 Ctrl-C | 바로 끝낸다: 마지막 commit과 꼬리가 남는다 | 같다(시작 전 상태와 꼬리) |
| ipc `cancel` | 시작 전 상태로 되돌린다 | 같다 |
| SIGKILL, 정전 | 마지막 commit | 시작 전 상태 |

- **되돌리기는 공짜다.** append-only이므로 시작할 때의 길이로 truncate하면 된다. `-c`가 만든 파일은
  지운다.
- **`--atomic`은 전부 아니면 전무다.** `-a`는 실행 내내 처음의 footer를 살려 두고, 끝에 한 번만 죽인다.
  중간 segment는 쓰되 footer는 마지막 것만 쓴다. 중간에 죽으면 시작 전 상태와 꼬리가 남는다. `-c`는
  지금처럼 `.part`에 쓰고 끝에 rename한다.
- **꼬리**는 다음 `-a`가 처리한다(앞으로 진행하거나 잘라 낸다). 읽는 쪽은 경고만 한다.

## 출력 이름 (`-c`)

- 출력이 없던 경우에는 `.part`에 쓰다가 **첫 commit에서** 최종 이름으로 rename한다. 그 뒤로는 그 자리에서
  덧붙인다. commit 전에 멈추면 지금처럼 지운다.
- `-c -f`는 "대상을 빈 아카이브로 바꾸고 `-a`"이므로 옛 파일은 첫 commit에서 바뀐다. 옛 파일을 끝까지
  지키려면 `--atomic`을 쓴다. 지금 `-f`가 보장하는 것(끝날 때까지 옛 파일이 그대로 있다)이 기본에서는
  사라진다는 점에 유의한다.
- 암호화하면(`-P`) `.part`에 쓰다가 끝에 rename한다(아래 "암호화된 아카이브").
- 단일 파일 아카이브(metadata block이 없는 경우)는 앞부분만 있으면 짧은 파일로 풀릴 뿐 실패하지 않는다.
  그래서 지금과 같이 `.part`를 쓰고 끝에 rename한다. 단일 파일 해제도 같다.

## `-a`의 의미

- `aqr -a ARCHIVE INPUTS…`(`--append`)는 갱신이다. 합친 트리와 비교해, 없거나 바뀐 엔트리를 새 segment에
  넣고 같은 것은 건너뛴다. "같다"의 판정은 경로, 종류, 크기, mtime이다(rsync의 quick check). 엔트리
  이름은 `-c`와 같은 규칙(입력, `-C`, rename)으로 만든다.
- `-a`는 지우지 않는다(zip의 `-u`처럼). 지우는 것은 별도 verb인 `aqr -s ARCHIVE INPUTS…`(`--sync`)가 한다.
  `-s`는 `-a`의 갱신에 더해, 합친 트리에 있지만 입력에 없는 것마다 그 subtree의 꼭대기에 tombstone을 쓴다.
  그 결과는 같은 입력으로 `-c`를 돌린 트리와 같다(quick check가 보는 한에서). 입력 밖의 최상위 이름도
  지워지고, 디렉터리 하나를 주면 `-c`처럼 그 내용이 최상위가 된다. 입력 디렉터리 아래만 지우는 방식
  (처음 구현했던 `-a --delete`)은 sync라는 이름에서 기대하는 바와 달라서 없앴다.
- **`-s`도 파일을 키운다.** 지워진 것은 tombstone으로 표시될 뿐이고 그 바이트는 남는다. 공간은
  `aqr --compact`가 되찾는다(segment 구조를 유지한 채 남은 블록만 복사해 다시 쓰고 rename한다;
  `crates/aqr-formula/src/aqr/rewrite.rs`). segment를 합치는 것(`--merge-segments` 같은 것)은 필요해지면 따로.
- **재개는 별도 기능이 아니다.** 멈춘 `-c`는 같은 입력으로 `aqr -a out.aqr …`를 돌리면 남은 파일이 "없는
  것"이라 들어간다. 결과는 중단 없이 만든 `-c`보다 조금 크다. 모듈이 segment마다 실리고, PNG set 같은
  묶음이 segment 경계에서 끊기기 때문이다.
- **`--rm`.** batch는 쓰기 전에 검증된다. 그러니 commit을 sync한 뒤 그 commit에 든 입력을 지운다. 멈추면
  commit된 것까지만 지워지고, 다시 `-a --rm`을 돌리면 "옮기기"가 이어진다.
- **archive-wide link group 번호**는 기존 트리에서 쓴 번호를 피해 새로 매긴다.

## 표식: `segment-status`

"commit했지만 실행이 끝나지 않았다"와 "실행이 끝났다"는 서로 배타적이다. 그래서 키를 새로 만들지 않고
-3의 이름과 값을 넓힌다(spec, "`last-segment-intended`"를 이 절로 바꾼다).

```cddl
aqr-segment-status = (
  ? &(segment-status: -3) => true / aqr-incomplete
)

aqr-incomplete = {
  ? &(files: 0) => uint  ; Files left out
  ? &(bytes: 1) => uint  ; Their bytes
}
```

| 값 | 뜻 | 언제 |
| --- | --- | --- |
| `true` | 실행이 끝난 commit | 끝까지 간 실행의 마지막 segment. 지금의 `last-segment-intended`와 같다 |
| map | 실행이 끝나지 않은 commit, 남은 것 | 주기적 commit과 멈출 때의 commit |
| 없음 | commit의 끝이 아닌 segment | `--atomic`이나 암호화 아카이브의 중간 segment |

- **아카이브의 상태는 마지막 segment의 값이다.** -3은 지금처럼 archive value의 extra로 들어가지 않는다
  (해석 9.3단계가 제외한다). 그러니 다음 commit이 앞의 값을 지울 필요가 없다.
- **잡을 수 없는 중단 뒤에도 정직하다.** 남는 것이 주기적 commit이고, 그 commit에 map이 적혀 있기 때문이다.
- **footer 재구성.** 재구성이 끝난 자리의 metadata block에 값이 없으면 지금처럼 뒤의 segment를 잃었을 수
  있다고 알린다. map이면 미완성으로 알리고, `true`면 지금과 같다.
- **호환성.** 지금의 아카이브에는 -3 ⇒ `true`가 있고 뜻이 그대로다. 지금의 디코더는 map 값을 거절하므로
  (`tree.rs`: "last-segment-intended not true") 같은 변경에서 받게 한다.
- **디코더가 map을 보면.** `-I`는 "incomplete: N files (B) not archived; `aqr -a` to resume"을 보여 준다.
  `-t`와 `-d`는 경고하되 성공으로 끝낸다. 아카이브 자체는 온전하기 때문이다.

## 암호화된 아카이브

덧붙이기를 허용하지 않는다. age STREAM의 마지막 청크에는 flag가 붙어 있어서, 이어 쓰려면 같은 키와 같은
청크 번호로 그 청크를 다시 암호화해야 한다. 덧붙인 양이 그 청크 안에서 끝나거나 덧붙이기가 중단돼 다시
하면, (번호, flag)가 같은 nonce로 다른 평문을 암호화하게 된다(키스트림 재사용). 옛 footer를 죽이는 2바이트도
이미 암호화한 청크 안에 있으므로 쓸 수 없다.

- `-a`가 암호화된 아카이브를 만나면 거절한다. `-a -P`도 거절한다. 비암호화 아카이브에 암호화된 segment를
  덧붙이는 것은 뜻이 없기 때문이다.
- `-c -P`는 commit을 중간에 하지 않는다. segment는 닫되 footer는 쓰지 않고(값 없음), `.part`에 쓰다가 끝에
  rename한다. 멈추면 열린 segment를 닫고 sealer를 `finish`해서(마지막 청크의 flag) 올바른 아카이브로
  남긴다. 잡을 수 없는 중단에 대해서는 지금처럼 repair(손상된 암호화 아카이브의 복호와 footer 재구성)에
  기댄다.
- 증분 백업을 암호화하려면 별도 설계가 필요하다. 증분마다 따로 암호화한 overlay 아카이브를 두는 사슬이나,
  commit마다 새 `payload_nonce`로 STREAM을 잇는 spec 확장이 후보다.

## 비용과 남은 문제

- **덧붙일 때마다 드는 고정 비용.** segment마다 모듈, 리소스, 사전이 실린다. 작은 증분이 잦으면 이 비용이
  지배한다. 등록된 모듈의 shorthand block(해시 참조)과 계획 중인 내장 사전(CLAUDE.md의 Modules 절)이
  이 비용을 줄인다.
- **segment를 넘는 내용 공유(추가 작업).** 옮기거나 이름을 바꾼 파일, mtime만 바뀐 파일은 내용을 다시
  저장한다. internal import(블록 58–5F)가 있어야 피할 수 있는데, 디코더가 아직 받지 않는다(CLAUDE.md의
  Project notes). 이 설계의 범위가 아니며, 증분 백업을 실용적으로 만드는 다음 작업으로 남긴다.
- **다른 형식(aqr-pack).** 범위 밖이다. 나중에 tar는 end block을, ZIP은 central directory를 쓰는 것으로
  같은 stop 경로에 붙인다.

## UI

- `-a, --append`, `-s, --sync`, `--atomic`.
- 첫 Ctrl-C에서는 다음을 보여 준다.
  `stopping: committing the 1,234 files written (3.2 GB of 9.8 GB) to out.aqr; Ctrl-C again to stop at once`
  끝나면 `stopped: out.aqr holds 1,234 of 5,678 files; aqr -a to add the rest`를 보여 주고 exit 130으로
  끝난다.
- 출력이 이미 있어서 `-c`가 실패할 때, 그 파일에 표식이 있으면 메시지가 `-a`로 이어 쓸 수 있다고 알린다.
- `--ipc`: `hello`의 `controls`를 `["cancel","finish"]`로 하고, 진행 이벤트에 commit된 파일 수를 넣는다.
  `stopped` 이벤트에는 `"kept": {files, bytes}` 또는 null을 넣는다. aqr-win·aqr-mac의 취소는 commit된
  것이 있을 때만 "지금까지의 N개를 남길까요?"(남기기 / 버리기 / 계속)를 묻는다. 문구는 aqr-l10n에 둔다.

## 기본값

멈추면 commit하는 것을 기본으로 하고, `--atomic`은 옵트인으로 둔다.

- 이 모델에서는 잡을 수 없는 중단도 마지막 commit을 남긴다. Ctrl-C만 지운다면 일관성이 없다.
- 중단되지 않으면 비용이 0이다. 몇 시간치를 잃는 것이 파일 하나를 지우는 것보다 훨씬 나쁘다.
- 남는 것은 정직하다. 아카이브로 올바르고, 표식이 있고, exit 130으로 끝난다. 그리고 `-a`로 이어 쓸 수
  있다.

## 해제 (디렉터리)

기본 동작으로 하고, 덧붙이기와는 독립이다. 빈 파일과 잘린 파일은 어떤 경우에도 쓸모가 없다.

1. 첫 신호에서 flag를 세운다. 새 leaf는 시작하지 않는다.
2. `Extractor::stop`이 `s`의 write lock을 잡는다. `content`는 쓰는 동안 read lock을 쥐고 있으므로 진행 중인
   쓰기가 끝나고 새 쓰기는 막힌다. 큰 content는 예컨대 64 MiB 단위로 나눠 쓰면서 flag를 확인하고, 끝까지
   쓰지 못하면 미완성으로 친다.
3. 내용을 다 쓰지 못한 파일을 지운다. 빈 자리표시 파일과 그 hard link, 미완성 fork가 든다. 기준은
   `written[c]`다.
4. 받은 symlink를 `finish`와 같은 검사로 만든다. 끝까지 쓴 파일의 metadata를 적용하고, 디렉터리 metadata를
   가장 깊은 것부터 적용한다.
5. `stopped: 1,234 of 5,678 files extracted into DIR; 3 unfinished removed`를 보여 주고 130으로 끝난다.
   `--rm`이면 아카이브를 남긴다.

## 시험

- 결정적으로 멈추도록 hook을 둔다(시험용 env `AQR_TEST_STOP_AFTER=N`은 지금 해제에만 있다. 압축은
  `crates/aqr/tests/append.rs`가 실제 신호로 멈춘다.
- **정렬.** 아카이브를 홀수 오프셋에 둔 파일(stub을 앞에 붙임)로 시험한다. 새 footer가 `p % 512 = 511`에 오는
  경우 spacer로 옮기는지, 이미 그 자리에 있는 footer에는 예전 방식으로 물러나는지 본다.
- **명세 경로.** 공존 상태(완결된 꼬리, commit 전)가 옛 상태로 읽히는지 본다. random access와 앞에서부터
  읽기가 같은 결과를 내야 한다. 꼬리가 붙은 경우 마지막 commit이 보여야 한다. 찢어진 footer(앞부분만,
  0 채움)는 footer로 읽히지 않아야 한다. footer 뒤에 다른 아카이브가 이어 붙은 경우는
  지금과 같아야 한다. 각각 aqr-formula에서 시험한다.
- **`-a`.** 더하기, 바꾸기, `-s`(`-c`와 같은 트리), 같은 입력으로 다시 돌리면 아무것도 쓰지 않는지를 본다.
- **멈춘 `-c`.** `-a`로 재개한 결과가 중단 없는 `-c`와 같은 트리를 주는지 본다.
- **SIGKILL.** commit 사이에 죽인다(시험에서 프로세스를 죽인다). 마지막 commit이 보이고, 다음 `-a`가
  꼬리를 처리해야 한다. 각 쓰기 사이에서 죽이는 fault injection(쓰기를 가로채는 출력)으로 위의 표를
  전부 확인한다.
- **`--atomic`.** 무엇이 오든 시작 전 상태여야 한다.
- **해제.** 빈 파일이나 잘린 파일이 없어야 한다.
- 회귀 시험은 고치기 전에 실패하는 것을 먼저 확인한다.

## 작업 순서

1–5는 했다(위 "구현"). 6–7이 남았다.

1. spec: Locate의 공존 상태 확인, compressor의 의무, `segment-status`(-3의 이름과 값), Repair의 알림.
2. aqr-formula: 읽기(locate의 확인, 합친 트리, `segment-status`), 쓰기(commit마다 footer, 정렬, 패딩, 옛
   footer 죽이기).
3. aqr-compress: 앞선 상태에서 시작하는 엔진, commit hook(sync), 멈출 때의 commit 경로.
4. aqr: `-a`(트리 비교, `-s`, lock, 꼬리 처리), `stop.rs`의 단계, `--atomic`, 출력 이름,
   `--rm`, `--ipc`.
5. 해제 정리.
6. 추가 작업: internal import. 별도 설계: 암호화한 증분 백업.
7. 10gb에서 commit의 비용과 멈출 때의 지연을 잰다(Mac. 이 클라우드 머신에는 데이터가 없다).

## 저널링: `-j`와 `--at`

덧붙이기로 쌓인 segment에서 "돌아갈 상태"를 사용자가 이름 붙이는 기능이다. 포맷은 spec의 `journal`
(절 "`journal`")이 정한다. 여기서는 UI와 구현의 결정만 적는다.

- **지점은 명시적이다.** commit은 주기적 commit과 멈출 때의 commit도 만들고 segment는 `-S`로도 나뉘므로,
  이들을 복원 지점으로 삼지 않는다. `aqr -c|-a|-s -j LABEL …`만 그 실행의 마지막 commit
  (`segment-status: true`)에 `journal: {now_ms => LABEL}`을 쓴다. 라벨은 필수이고 빈 문자열(`-j ''`)도 된다.
  중단된 실행은 지점을 만들지 않는다.
- **변화가 없어도 지점을 남긴다.** `-a`/`-s`가 쓸 것이 없으면 엔트리 없는 segment 하나에 `journal`만 담아
  덧붙인다. 마지막 segment를 고쳐 쓸 수 없는 것은 append-only와 크래시 안전성 때문이다(옛 footer를 죽이는 commit만
  원자적이다). 비용은 metadata block 하나다. spec은 첫 segment가 아니면 엔트리 0개를 허용한다.
- **`--compact`.** 지점이 없는 아카이브는 지금과 같다. 지점이 있으면 이력(지워진 것, 덮어쓴 것)을 버리므로
  `--keep-since SPEC`이나 `--drop-history`를 요구하고, 둘 다 없으면 지점 수를 알리며 거절한다.
  - `--drop-history`: 지금의 `--compact`(마지막 상태만)에 지점을 모두 버린다. `--delete`/`--move`와도 쓴다.
  - `--keep-since SPEC`(`--at`과 같은 선택자): 고른 지점 P부터의 지점과 그 상태를 남긴다. P의 segment까지는
    P의 트리 하나로 접어 지금과 같은 방식으로 다시 쓰고(P의 segment에서 P 이후의 지점은 맵째 옮긴다),
    그 뒤의 segment는 블록을 모두 그대로 복사하고 metadata block만 새 위치에 맞춰 다시 쓴다(엔트리,
    tombstone, `segment-status`, `journal`은 그대로). `--delete`/`--move`와는 함께 쓰지 않는다. 사이 상태의
    트리가 바뀌기 때문이다. 끝난 뒤 마지막 상태가 처음과 같은지는 기존 검사(`checked`)가 본다.
  - 한계: 뒤 segment의 엔트리 extra가 "있지만 비어 있는 맵"(`{}`)이면(spec, "Merging segments"의 교체 표기)
    다시 쓸 수 없어 거절한다. 빈 segment의 사슬을 한 segment로 합치는 일은 아직 하지 않는다.
- **라벨의 placeholder.** `{today}`는 지역 날짜 `YYYY-MM-DD`, `{now}`는 지역 시각 `YYYY-MM-DD HH:MM:SS`다.
  `{{`와 `}}`는 괄호 자체이고, 모르는 이름은 오류다. 시각 키(UTC ms)와는 별개로 라벨 문자열만 바꾼다.
- **`--at SPEC`**(`-t`, `-I`, `-d`, 이후 `--diff`)은 지점 중에서만 고른다.
  - `#N`: N번째 지점(1부터, segment 순서). `#-N`은 끝에서 N번째이다.
  - `@LABEL`: 그 라벨의 마지막 지점.
  - 날짜: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY-MM-DDTHH[:MM[:SS]]`. 모자란 자리는 그 기간의 끝까지로
    읽고(`2026-11-01` = 그 날의 23:59:59.999), 시간대는 지역이며 `Z`나 `+09:00`을 붙이면 따른다.
    `time ≤ 그 끝`인 마지막 지점을 쓴다.
  - 해당 지점이 없으면 오류이고, 가장 이른 지점을 알린다. 지점이 하나도 없으면 "no journal points".
- **`--history`**는 지점 목록(번호, 시각, 라벨)을 보인다. `-L`은 이미 `--dereference`라서 짧은 이름은 없다.
  엔트리 수와 크기는 아직 보이지 않는다.
- 시각은 벽시계(UTC)만 쓰고, 시험에서는 `AQR_TEST_NOW=ms`로 고정한다.

- **`--diff`**는 두 상태의 차이를 한 줄씩 보인다(`+` 추가, `~` 바뀜, `-` 삭제; 디렉터리는 `/`를 붙이고
  메타데이터는 보지 않는다). 파일은 크기와 메타데이터로 가른다(`-a`의 quick check와 같다). 상태는 `--from`과
  `--at`(둘 다 `--at`의 선택자)이 고른다. 기본은 마지막 지점과 지금의 비교이고, `--at`만 주면 그 지점과
  그 앞 지점의 비교(앞이 없으면 빈 상태), `--from`은 앞 상태를 바꾼다. 내용 바이트는 풀지 않으므로 크기와
  mtime이 같게 내용만 바뀐 파일은 잡지 못한다.
  `aqr --diff ARCHIVE PATH...`은 아카이브(`--at`이 있으면 그 상태)와 경로를 비교한다(zpaq의 `compare`처럼).
  경로는 `-a`의 입력과 같은 규칙(`-C`, `-x`, `-L`, 디렉터리 하나면 그 내용이 최상위)으로 모으고, `-a`의
  quick check(크기, mtime)로 가른다. `+`는 경로에만 있는 것(`-a`가 더할 것), `-`는 아카이브에만 있는 것
  (`-s`가 지울 것), `~`는 다른 것이다. `--from`과는 함께 쓰지 않는다.

구현한 것(`-j`, `--at`로 `-t`/`-d`/`-K`, `--history`, `--diff`): 중단된 실행은 지점을 만들지 않고, `-s`/`-a`가 바꿀 것이
없어도 빈 segment로 지점을 남긴다. `--at`은 `-I`에는 아직 없다. `--compact`는 `--keep-since`나 `--drop-history`를 요구한다. 한 줄 요약의 `-j`는 단일 파일 아카이브, `--format`의 다른
형식, 암호화 아카이브(덧붙일 수 없다)와 함께 쓸 수 없다.

작업 순서: (1) spec, (2) aqr-formula의 읽기(지점, 지점까지의 접두 트리)와 쓰기(`journal`, 빈 segment),
(3) aqr의 `-j`, `-L`, `--at`, (4) `--compact`의 이력 규칙, (5) `--diff`, `--ipc`, 프런트엔드.

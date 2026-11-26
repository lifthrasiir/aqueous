# 덧붙이기(`-a`)와 중단

증분 백업에는 아카이브에 segment를 덧붙이는 `-a`가 필요하다. 이 설계는 `-c`를 "빈 아카이브를 만들고
`-a`를 돌리는 것"으로 본다. `-a`는 덧붙이기만 하는 트랜잭션이다. 덧붙이는 것 말고 고치는 것은 옛 footer의
2바이트뿐이고, commit은 언제든 할 수 있다. 그러면 어디서 멈추든 마지막 commit까지 담은 올바른 아카이브가
남고, 같은 `-a`로 이어 쓸 수 있다. 이 성질은 footer 재구성(spec의 "Repair")에 기대지 않는다.

구현은 끝났다(2026-11-05; spec, aqr-formula, aqr-compress, aqr 순). 경과는 git 이력에 있고, 이 문서에는
근거와 남은 일만 둔다. 포맷은 spec의 "Appending to an archive", "trim a located archive", `segment-status`,
"Unencrypted archive", "Locating an archive", "Interpreting an archive"가 정한다. 구현의 자리는
`crates/aqr/src/{append,stop,journal,rollback}.rs`, aqr-formula의 `locate`·`trim`·`footer_of`·`SegmentWriter`,
aqr-compress의 `Output`·`Stop`·`Appending`, `crates/aqr-archive/src/rewrite.rs`(`--compact`), 시험은
`crates/aqr/tests/{append,journal}.rs`. segment를 닫는 정책과 잘림(E4)의 논의는 `recovery-design.md` §4–5.

## 정한 것

- commit은 옛 footer를 2바이트 spacer header로 죽이는 것이다(아래 "footer를 옮기는 법").
- 멈추면 commit하는 것이 기본이고, 전부 아니면 전무인 `--atomic`을 옵트인으로 둔다.
- `-a`는 갱신(없거나 바뀐 것만)이고 아무것도 지우지 않는다. tombstone은 동기화 verb `-s`(`--sync`)만 쓴다.
- 암호화된 아카이브에는 덧붙이지 않는다(nonce 재사용).
- `last-segment-intended`(-3)를 `segment-status`로 바꿔 "미완성"도 함께 나타낸다(CDDL과 값의 뜻은 spec).
- `-c` ≡ 빈 아카이브 + `-a`: 엔진은 "앞선 상태"(끝 오프셋, 합친 트리, segment 수, archive-wide link group
  번호)를 받고, `-c`는 그것이 빈 경우다. 빈 아카이브(header와 footer뿐)는 빈 파일 하나로 풀린다.

## 남은 일 (2026-11-27 코드에서 확인)

- **프론트엔드의 `finish`.** 프로토콜과 `aqr_ipc::Ipc::finish`(`FINISH`)만 있고 aqr-win/aqr-mac에는 그것을 보낼
  버튼이나 대화상자가 없다(aqr-win은 `--atomic`의 "Keep nothing if stopped" 옵션만 있다). "지금까지의 N개를
  남길까요?"(남기기 / 버리기 / 계속) 질문도 아직 없다.
- **`--rm`과 중단.** 멈추면 입력을 하나도 지우지 않는다. commit마다 지우는 것(commit을 sync한 뒤 그 commit에 든
  입력만 지움)은 하지 않았다.
- **다른 형식의 해제(aqr-unpack)**는 멈추면 이전처럼 끝난다. 큰 content를 나눠 쓰면서 그 안에서 멈추는 것도
  하지 않았다. 해제의 정리는 다 쓰지 못한 fork가 있는 파일을 남긴다(파일 자체는 온전하다).
- **다른 형식의 압축(aqr-pack)**: 범위 밖. 나중에 tar는 end block을, ZIP은 central directory를 쓰는 것으로 같은
  stop 경로에 붙인다.
- **Windows**: 파일 잠금(`File::try_lock`)과 콘솔 닫힘의 처리는 type check만 하고 실행해 보지 않았다.
- **10gb에서 commit의 비용과 멈출 때의 지연**은 Mac에서 재야 한다(이 머신에서 16 MB 시험 디렉터리를
  `-T 1 -B 100K -S 1`로 만들면 SIGTERM부터 끝까지 50 ms 안에 끝났다).
- **internal import**(블록 58–5F)가 디코더에 없다(`docs/crates.md`). 이것이 없으면 옮기거나 이름을
  바꾼 파일, mtime만 바뀐 파일의 내용을 다시 저장한다. 증분 백업을 실용적으로 만드는 다음 작업.
- **암호화한 증분 백업**: 별도 설계가 필요하다. 증분마다 따로 암호화한 overlay 아카이브의 사슬이나, commit마다
  새 `payload_nonce`로 STREAM을 잇는 spec 확장이 후보.
- **시간으로 닫는 segment**(예: 10분): `-S`를 크기로 줄이면 비용이 크므로(`recovery-design.md` §5.1) 나중에
  고려한다.
- **segment마다 드는 고정 비용**(모듈, 리소스, 사전). 작은 증분이 잦으면 지배한다. 등록된 모듈의 shorthand
  block과 계획 중인 내장 사전(`docs/modules.md`)이 줄인다.
- **`-I`의 `--at`**과 `--history`의 엔트리 수·크기는 아직 없다(저널링 절). `--merge-segments` 같은 segment 합치기도
  필요해지면 따로.
- 시험 가운데 비어 있을 수 있는 것: 각 쓰기 사이에서 죽이는 fault injection으로 아래 표의 모든 행을 확인하는
  것(현재 `tests/append.rs`는 실제 신호와 SIGKILL을 쓴다).

## footer를 옮기는 법

덧붙이려면 footer를 뒤로 옮겨야 한다. **두 footer가 잠시 공존하게 두고, 옛 footer를 2바이트로 죽이는 것을
commit으로 삼는다.** 원자적인 큰 쓰기는 필요 없고 순서만 맞으면 된다. 순서는 spec의 "Appending to an
archive"가 정한다: 옛 footer F_k 뒤에 segment의 블록, metadata block M_{k+1}, 새 footer F_{k+1}을 쓰고 sync한
뒤, F_k의 첫 2바이트를 spacer header로 덮어 sync하고(commit), 나머지를 0으로 채운다. F_{k+1}을 쓰는 동안은
처음 만나는 footer가 F_k이므로 읽는 사람에게는 옛 상태가 보인다. 새 블록은 모두 F_k의 끝 뒤에 쓰므로 밀어내는
크기에 하한이 필요 없다.

### 전제와 엣지 케이스

commit은 쓰기 한 번이 찢어지지 않는다는 데 기댄다.

- **섹터.** 장치는 512바이트(또는 그 배수) 섹터를 원자적으로 쓴다(NVMe AWUPF ≥ 1 LBA, CoW 파일 시스템은 블록
  단위). 길이 h인 header를 **파일** 오프셋 p에 쓰는 일은 `p % 512 ≤ 512 − h`일 때 원자적이다(더 큰 섹터에도
  성립). 네트워크 파일 시스템(NFS, SMB)은 서버에 달려 있어 보장 밖이다.
- **정렬은 파일 기준이다.** SFX처럼 시작이 홀수일 수 있고 나중에 stub이 앞에 붙을 수도 있어 spec이 보장할 수
  없다. 덧붙이는 쪽이 그때의 파일 오프셋으로 판단한다.
  - **새 footer는 writer가 정렬한다.** F_{k+1}이 조건을 어기면 M_{k+1} 뒤에 2바이트 spacer(`00 00`)를 넣어
    옮긴다(1/256 확률로 2바이트). 그래서 Locate의 확인은 metadata block 뒤의 spacer block을 건너뛰고 본다.
  - **이미 있는 footer**가 조건을 어기면(512분의 1 남짓) 2바이트로 죽일 수 없다(찢어진 상태가 엉뚱한 블록
    경계나 모르는 critical type이 된다). 이때의 첫 commit만 예전 방식(footer를 잘라 내고 이어 쓰기)으로 하고
    그 사이의 중단은 footer 재구성에 맡긴다. 아카이브마다 한 번이고 `-v`로 알린다.
- **footer의 길이.** 죽이는 header의 길이는 spec이 정한다(F_k가 257바이트 이하면 2바이트). aqr의 footer는
  30바이트 이하라 늘 2바이트.
- **거절하는 경우**: 아카이브 뒤에 우리 것이 아닌 바이트가 있을 때(서명, 전송이 붙인 것; 우리 것은 완결되었거나
  쓰다 만 commit), 암호화된 아카이브, size hint block이 있는 아카이브(제자리에서 못 고침; aqr는 암호화할 때만
  쓴다). overlay 아카이브의 footer(`OVERLAY_FOOTER_TYPE`)도 똑같이 죽인다.

| 어디서 죽었나 | 남는 것 |
| --- | --- |
| 블록이나 M_{k+1}을 쓰는 중 | 옛 상태와 꼬리 |
| F_{k+1}이 찢어졌거나 0으로 채워졌을 때 | 옛 상태와 꼬리. F_k가 처음 footer다 |
| F_{k+1}을 다 쓴 뒤, commit 전 | 옛 상태. 꼬리는 완결된 commit 하나다 |
| commit 뒤 | 새 상태 |

다음 `-a`는 꼬리를 잘라 낸다. 꼬리가 완결된 commit이어도 F_k를 죽여 앞으로 진행하지 않는다. 그때까지 파일은 옛
상태로 읽혔으므로 commit되지 않은 것이 나중에 보이면 읽은 쪽이 본 것이 바뀐다(spec은 둘 다 허용).

### 고려하고 버린 것

- **footer를 잘라 내고 이어 쓰기.** 새 footer를 쓰기 전까지 footer가 없어 재구성에 기댄다. 정렬이 안 맞는 옛
  footer의 첫 commit에만 쓴다.
- **중간의 footer를 영구히 건너뛰게 하는 spec 변경.** commit한 아카이브가 지금의 디코더에서 안 읽힌다.
- **새 footer를 멀리 예약해 두고 spacer 안에 쓴 뒤 spacer의 header를 바꾸기.** header를 바꾸는 순간 앞에서부터
  읽으면 M_{k+1}이 보이는데 끝 footer는 M_k를 가리켜(반대도 마찬가지) 한 번의 쓰기로 못 맞춘다. 이점은 쓰는
  동안에도 파일 끝이 올바른 footer라는 것 하나이고, 형식은 그대로 writer만 바꿔 얻을 수 있다(끝 footer를 멀리
  써 두고 commit 때 M_{k+1}을 가리키게 고친 뒤 guard footer를 두고 F_k를 죽임; 그 거리가 F_k 길이 이상이어야
  함). 넣지 않았다.

### 명세 쪽의 결정

- Locate는 한 commit이 여러 segment일 수 있어서(`--atomic`의 `-a`) 첫 metadata block까지 거슬러 올라간다.
- 아카이브에 footer는 끝에 하나뿐이다. 공존은 쓰는 도중에만 생기고, 남으면 다음 덧붙이는 쪽이 정리한다.
- Repair는 바뀌지 않는다. commit을 마친 아카이브의 옛 footer는 spacer이므로 E4에는 footer 재구성이 필요하다.
- 호환: commit을 마친 아카이브는 옛 디코더도 읽는다. 옛 디코더가 공존 상태를 읽으면 잘못 읽는 대신 실패한다.

## commit 규약

1. `-a`는 아카이브를 열고 advisory lock(`flock`, `LockFileEx`)으로 쓰는 쪽을 하나로 묶는다. 끝을 찾고 꼬리가
   있으면 잘라 낸다.
2. 블록과 metadata block을 쓰고 sync, footer를 쓰고 sync, 옛 footer를 죽이고 sync, 남은 바이트를 0으로
   채운다. macOS의 `fsync`는 순서를 보장하지 않으므로 중간 sync에는 `F_BARRIERFSYNC`, commit에는
   `F_FULLFSYNC`.
3. commit 하나의 비용은 footer 20바이트 남짓, 패딩 1바이트 이하, sync 셋(10gb에서 segment 7개라 무시할 만).

### commit 지점

- **주기적 commit**은 segment를 닫을 때(`-S`, 기본 256M; 암호화하면 없음). 잡을 수 없는 중단에서 잃는 것은 이
  단위다.
- **멈출 때의 commit**은 열린 segment를 그때까지 쓴 batch로 닫는다. 다 쓴 batch는 블록이 온전하고 빠진 것은
  metadata block과 footer뿐이다. 중단되지 않으면 비용 0, 잃는 것은 진행 중이던 batch(`AHEAD` = 2, 입력 1 GiB
  남짓)뿐이다. 첫 신호에서 handler는 atomic flag만 세우고, writer는 `LOOK`(10 ms)마다 깨어 flag를 본다. 배치를
  만드는 일은 writer가 아닌 스레드에서 돌아 writer는 계산 중인 batch를 기다리지 않는다. commit되는 것은
  `write_all`을 마친 batch의 content이고(`same_as`인 content는 대상도 commit됐을 때만), metadata는 스레드 1로
  인코딩한다(pool은 버려진 batch로 바쁨). 분석 중인 스레드는 join하지 않고 callback이 요약과 `--stats`를 쓰고
  130으로 `exit`한다(`stop::now`).

## 중단의 의미

| 무엇이 왔나 | 기본 | `--atomic` |
| --- | --- | --- |
| 첫 Ctrl-C, SIGTERM, SIGHUP, ipc `finish` | 열린 segment를 닫아 commit하고 exit 130 | 시작 전 상태로 되돌린다 |
| 두 번째 Ctrl-C | 바로 끝낸다: 마지막 commit과 꼬리가 남는다 | 같다(시작 전 상태와 꼬리) |
| ipc `cancel` | 시작 전 상태로 되돌린다 | 같다 |
| SIGKILL, 정전 | 마지막 commit | 시작 전 상태 |

- 되돌리기는 공짜다. append-only이므로 시작할 때의 길이로 truncate한다(`-c`가 만든 파일은 지운다).
- `--atomic`: `-a`는 내내 처음의 footer를 살려 두고 끝에 한 번만 죽인다(중간 segment는 쓰되 footer는 마지막
  것만). `-c`는 `.part`에 쓰고 끝에 rename한다.
- 꼬리는 다음 `-a`가 잘라 낸다. 읽는 쪽은 경고만 한다.
- **기본이 commit인 이유**: 잡을 수 없는 중단도 마지막 commit을 남기므로 Ctrl-C만 지우면 일관성이 없고,
  중단되지 않으면 비용 0이며(몇 시간치를 잃는 것이 파일 하나를 지우는 것보다 훨씬 나쁘다), 남는 것은 아카이브로
  올바르고 표식이 있고 exit 130이며 `-a`로 이어 쓸 수 있다.

## 출력 이름 (`-c`)

- 출력이 없던 경우 `.part`에 쓰다가 **첫 commit에서** 최종 이름으로 rename하고 그 뒤로는 그 자리에서 덧붙인다.
  commit 전에 멈추면 지운다.
- `-c -f`는 "대상을 빈 아카이브로 바꾸고 `-a`"라 옛 파일은 첫 commit에서 바뀐다. 옛 파일을 끝까지 지키려면
  `--atomic`(예전 `-f`가 보장하던 "끝날 때까지 옛 파일 그대로"는 기본에서 사라졌다).
- 암호화(`-P`)와 단일 파일 아카이브(metadata block이 없어 앞부분만 있으면 짧은 파일로 풀릴 뿐 실패하지 않음)는
  `.part`에 쓰고 끝에 rename한다. 단일 파일 해제도 같다.
- 잡을 수 없는 중단에서 `.part`는 남고, 닫힌 segment까지는 footer 재구성으로 읽힌다.

## `-a`의 의미

- `aqr -a ARCHIVE INPUTS…`는 갱신이다. 합친 트리와 비교해 없거나 바뀐 엔트리를 새 segment에 넣고 같은 것은
  건너뛴다. "같다"는 경로, 종류, 크기, mtime(rsync의 quick check). 엔트리 이름은 `-c`와 같은 규칙이다.
- `-a`는 지우지 않는다(zip의 `-u`처럼). 지우는 것은 `aqr -s ARCHIVE INPUTS…`(`--sync`)로, 합친 트리에 있지만
  입력에 없는 것마다 그 subtree의 꼭대기에 tombstone을 쓴다. 결과는 같은 입력의 `-c`와 같은 트리이고, 입력 밖의
  최상위 이름도 지워지며 디렉터리 하나를 주면 `-c`처럼 그 내용이 최상위가 된다. (입력 디렉터리 아래만 지우던
  `-a --delete`는 sync라는 이름의 기대와 달라 없앴다.)
- `-s`도 파일을 키운다(tombstone일 뿐). 공간은 `aqr --compact`가 되찾는다(segment 구조를 유지해 남은 블록만
  복사해 다시 쓰고 rename; `rewrite.rs`).
- 재개는 별도 기능이 아니다. 멈춘 `-c`는 같은 입력으로 `aqr -a out.aqr …`를 돌리면 남은 파일이 "없는 것"이라
  들어간다. 결과는 중단 없는 `-c`보다 조금 크다(모듈이 segment마다 실리고 PNG set 같은 묶음이 segment 경계에서
  끊김).
- `--rm`: batch는 쓰기 전에 검증되므로 commit을 sync한 뒤 그 commit의 입력을 지울 수 있다(남은 일 참조).

## 암호화된 아카이브

덧붙이기를 허용하지 않는다. age STREAM의 마지막 청크에는 flag가 붙어 있어서 이어 쓰려면 같은 키와 같은 청크
번호로 그 청크를 다시 암호화해야 하고, 덧붙인 양이 그 청크 안에서 끝나거나 덧붙이기가 중단돼 다시 하면 (번호,
flag)가 같은 nonce로 다른 평문을 암호화한다(키스트림 재사용). 옛 footer를 죽이는 2바이트도 이미 암호화한 청크
안에 있다.

- `-a`가 암호화된 아카이브를 만나면, 그리고 `-a -P`도 거절한다.
- `-c -P`는 commit을 중간에 하지 않는다. segment는 닫되 footer는 쓰지 않고(값 없음) `.part`에 쓰다 끝에 rename.
  멈추면 열린 segment를 닫고 sealer를 `finish`해(마지막 청크의 flag) 올바른 아카이브로 남긴다. 잡을 수 없는
  중단은 repair(손상된 암호화 아카이브의 복호와 footer 재구성)에 기댄다.

## `segment-status`

CDDL과 값의 뜻(`true` = 실행이 끝난 commit, map `{files, bytes}` = 끝나지 않은 commit과 남은 것, 없음 = commit의
끝이 아닌 segment)은 spec. 이 절은 결정만 둔다.

- 아카이브의 상태는 마지막 segment의 값이다(-3은 archive value의 extra로 들어가지 않는다. 해석 9.3단계가 제외).
  그래서 다음 commit이 앞의 값을 지우지 않는다.
- 잡을 수 없는 중단 뒤에도 정직하다(남는 것이 주기적 commit이고 map이 적혀 있다). 재구성이 끝난 자리에 값이
  없으면 뒤의 segment를 잃었을 수 있다고 알리고, map이면 미완성, `true`면 지금과 같다.
- 옛 디코더는 map을 거절한다(`tree.rs`: "last-segment-intended not true").
- 디코더: `-I`는 "incomplete: N files (B) not archived; `aqr -a` to resume"을 보이고, `-t`와 `-d`는 경고하되
  성공으로 끝낸다(아카이브 자체는 온전하므로).

## UI

- `-a, --append`, `-s, --sync`, `--atomic`. 첫 Ctrl-C에서
  `stopping: committing the 1,234 files written (3.2 GB of 9.8 GB) to out.aqr; Ctrl-C again to stop at once`,
  끝나면 `stopped: out.aqr holds 1,234 of 5,678 files; aqr -a to add the rest`와 exit 130. 출력이 이미 있어서
  `-c`가 실패할 때 그 파일에 표식이 있으면 메시지가 `-a`로 이어 쓸 수 있다고 알린다.
- `--ipc`: `hello`의 `controls`는 `["cancel","finish"]`, 진행 이벤트에 commit된 파일 수, `stopped` 이벤트에
  `"kept": {files, bytes}` 또는 null. aqr-win·aqr-mac의 취소가 commit된 것이 있을 때만 "남길까요?"를 묻는 것은
  아직 없다(남은 일).

## 해제 (디렉터리)

기본 동작이고 덧붙이기와 독립이다(빈 파일과 잘린 파일은 어떤 경우에도 쓸모가 없다). 첫 신호에서 flag를 세워 새
leaf를 시작하지 않고, `Extractor::stop`이 `s`의 write lock을 잡아(`content`는 쓰는 동안 read lock) 진행 중인
쓰기가 끝나게 한 뒤, 내용을 다 쓰지 못한 파일(빈 자리표시 파일과 hard link, 미완성 fork; 기준은 `written[c]`)을
지우고, 받은 symlink와 끝까지 쓴 파일의 metadata, 디렉터리 metadata(가장 깊은 것부터)를 적용한다.
`stopped: 1,234 of 5,678 files extracted into DIR; 3 unfinished removed`와 130. `--rm`이면 아카이브를 남긴다.
결정적 시험용으로 `-Z test:stop-after=N`(해제에만 있음)이 있다.

## 저널링 (Journaling): `-j`와 `--at`

덧붙이기로 쌓인 segment에서 "돌아갈 상태"를 사용자가 이름 붙이는 기능이다. 포맷은 spec의 `journal`(절
"`journal`")이 정하고, 구현은 `crates/aqr/src/{journal,rollback}.rs`, `aqr_archive::rollback`, `Layers::until`.
UI와 구현의 결정만 적는다.

- **지점은 명시적이다.** 주기적 commit, 멈출 때의 commit, `-S`로 나뉜 segment는 복원 지점이 아니다.
  `aqr -c|-a|-s -j LABEL …`만 그 실행의 마지막 commit(`segment-status: true`)에 `journal: {now_ms => LABEL}`을
  쓴다. 라벨은 필수이고 빈 문자열(`-j ''`)도 된다. 중단된 실행은 지점을 만들지 않는다. `-j`는 단일 파일
  아카이브, `--format`의 다른 형식, 암호화 아카이브와 함께 쓸 수 없다.
- **변화가 없어도 지점을 남긴다.** `-a`/`-s`가 쓸 것이 없으면 엔트리 없는 segment 하나에 `journal`만 담아
  덧붙인다(마지막 segment를 고쳐 쓸 수 없는 것은 append-only와 크래시 안전성 때문; 비용은 metadata block 하나,
  spec은 첫 segment가 아니면 엔트리 0개를 허용).
- **`--compact`**: 지점이 없으면 지금과 같다. 있으면 이력을 버리므로 `--keep-since SPEC`이나 `--drop-history`를
  요구하고, 둘 다 없으면 지점 수를 알리며 거절한다.
  - `--drop-history`: 마지막 상태만 남기고 지점을 모두 버린다(`--delete`/`--move`와도 쓴다).
  - `--keep-since SPEC`(`--at`과 같은 선택자): P부터의 지점과 그 상태를 남긴다. P의 segment까지는 P의 트리 하나로
    접어 다시 쓰고, 그 뒤의 segment는 블록을 그대로 복사하고 metadata block만 새 위치에 맞춰 다시 쓴다.
    `--delete`/`--move`와는 함께 못 쓴다(사이 상태의 트리가 바뀜). 한계: 뒤 segment의 엔트리 extra가 빈 맵 `{}`
    (spec "Merging segments"의 교체 표기)이면 다시 쓸 수 없어 거절한다.
  - 같은 상태를 다시 이름 붙이는 segment(블록·엔트리·tombstone 없이 끝난 실행의 끝인 것: 변화 없는 `-j`, `--tag`)의
    사슬은 `--keep-since`가 앞 segment 하나에 지점을 맵째 합쳐 쓴다(같은 시각이 겹치면 합치지 않음). 지점을 하나도
    버리지 않고 합치려면 `--keep-since #1`.
- **되돌리기: `--rollback SPEC`과 `--convert --keep-until SPEC`.** 고른 지점 P 뒤의 상태를 모두 버린다(P의
  segment 뒤의 이름 없는 segment도).
  - `--rollback`은 제자리에서 자른다(commit을 거꾸로 함): P의 metadata block 뒤에서 다음 commit이 spacer로 죽인
    footer를 찾아(그 자리, 길이, 첫 바이트가 `kill`의 것인 spacer) 본문을 쓰고 sync, 첫 2바이트를 써서
    sync(이것이 commit)한 뒤 그 footer 끝에서 파일을 자른다. 결과는 P 때의 파일과 바이트 단위로 같다. 죽인
    footer가 없으면(sector를 넘어 다음 commit이 덮어쓴 경우) metadata block 바로 뒤에 한 sector 안에 드는 footer를
    새로 쓴다(넘으면 2바이트 spacer를 먼저. 어느 쪽만 쓰여도 옛 상태). 열기는 `-a`와 같고, 암호화·미러 블록·앞에
    다른 데이터가 있는 아카이브는 거절하며 `--convert --keep-until`을 쓴다.
  - Windows는 다른 프로세스가 매핑한 파일을 자르지 못한다(`ERROR_USER_MAPPED_FILE`; 탐색기의 폴더 보기가
    `aqr --serve`로 열어 둔 경우). 5초 재시도 후에도 안 되면 뒤의 바이트를 꼬리로 남기고, 읽는 쪽은 빼고 읽으며
    다음 `-a`가 자른다.
  - `--convert --keep-until`은 P까지의 상태를 다른 파일(또는 rename으로 제자리)에 쓴다. `--keep-since`,
    `--drop-history`와 함께 쓰면 P까지의 이력에 적용한다.
  - 탐색기: 속성의 "버전" 탭의 "이후 버전 삭제…"(`aqr-win rollback-version ARCHIVE #N`)가 확인 뒤 `--rollback`하거나
    "새 아카이브로 저장"이면 `--convert --keep-until`로 `이름 (라벨).aqr`을 쓴다.
- **라벨의 placeholder.** `{today}`는 지역 날짜 `YYYY-MM-DD`, `{now}`는 지역 시각 `YYYY-MM-DD HH:MM:SS`. `{{`와
  `}}`는 괄호 자체이고 모르는 이름은 오류. 시각 키(UTC ms)와는 별개다.
- **`--at SPEC`**(`-t`, `-d`, `-K`, `--diff`; `-I`에는 아직 없음)은 지점 중에서만 고른다.
  - `#N`: N번째 지점(1부터, segment 순서), `#-N`은 끝에서 N번째. `@LABEL`: 그 라벨의 마지막 지점.
  - 날짜 `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY-MM-DDTHH[:MM[:SS]]`: 모자란 자리는 그 기간의 끝까지(`2026-11-01` =
    23:59:59.999), 시간대는 지역이며 `Z`나 `+09:00`을 따른다. `time ≤ 그 끝`인 마지막 지점.
  - 해당 지점이 없으면 오류(가장 이른 지점을 알림), 하나도 없으면 "no journal points".
  - 시각은 벽시계(UTC)만 쓰고 시험에서는 `-Z test:now=ms`로 고정한다.
- **`--history`**는 지점 목록(번호, 시각, 라벨)을 보인다(`-L`은 이미 `--dereference`라 짧은 이름 없음).
- **`--diff`**는 두 상태의 차이를 한 줄씩 보인다(`+` 추가, `~` 바뀜, `-` 삭제; 디렉터리는 `/`; 메타데이터는 안
  봄). 파일은 크기와 메타데이터로 가른다(`-a`의 quick check). `--from`과 `--at`이 상태를 고른다: 기본은 마지막
  지점과 지금, `--at`만이면 그 지점과 앞 지점(없으면 빈 상태), `--from`은 앞 상태를 바꾼다. 내용 바이트는 풀지
  않아 크기와 mtime이 같고 내용만 바뀐 파일은 못 잡는다. `aqr --diff ARCHIVE PATH...`은 아카이브(`--at`이 있으면 그
  상태)와 경로를 비교한다(zpaq의 `compare`처럼; 경로는 `-a`의 입력과 같은 규칙; `+`는 `-a`가 더할 것, `-`는 `-s`가
  지울 것; `--from`과는 함께 못 쓴다).
- **프런트엔드.** `--ipc`의 프로토콜은 바꾸지 않는다(한 번의 실행을 따르는 프로토콜이고 지점은 실행 밖의 조회).
  프런트엔드는 `aqr --history --json ARCHIVE`(`number`, `time`(ms), `label`)로 지점을 받아 보여 주고 고른 것을
  `-d --at #N --ipc`로 꺼낸다. `--diff --json`은 변경(`change`, `path`, `dir`)과 개수를 한 줄로 준다.
  `-I`는 이미 segment마다의 상태를 모두 보이고 `journal`도 그 segment의 archive-wide 멤버로 보인다.

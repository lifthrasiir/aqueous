# 스트리밍 인코더: 큰 파일을 압축할 때의 메모리

압축(검증 포함)과 해제의 메모리를 입력 크기와 무관하게 "동시에 도는 적용들의 작업 메모리 + 간선마다 버퍼 하나 +
클래스마다 블록 하나 + 상수"로 묶기 위한 설계. 형식은 바꾸지 않는다(part module과 ring 창은 명세 "Module
execution"). 끝낸 일의 경과(단계별 커밋과 중간 수치)는 git 이력에 있다.

## 지금의 구조 (2026-11-27)

- **스필 = 스테이징** (`aqr_formula::store`, `crates/aqr-compress/src/spill.rs`). 큰 버퍼는 출력과 같은 디렉터리의
  파일(`PREFIX._$pill$PID.N`)에 positioned write로 쓰고 읽기 전용 매핑으로 읽는다. 쓴 페이지는 시스템의 파일 캐시에
  있어 시스템이 써 내고 버린다. 길이를 모르는 스트림은 `store::Writer`(heap 8 MiB, 넘으면 영역을 4배씩 키움).
  매핑된 입력은 `store::known`으로 등록해 `store::find`가 복사 없이 공유한다. 레벨 2 미만은 스필이 없고 배치를
  만드는 대로 쓴다(`ed3e36be`).
- **봉인과 싱크** (`aqr_archive::sink`, `crates/aqr-compress/src/seal.rs`, `scanning.rs`의 `make_later`). 조상이 모두
  파일을 따로 만드는 공식(`files_apart`: 이미지, WIM, ar/tar/cpio, asar)의 사슬 안에서 내용물을 만들어지는 대로
  싱크의 (클래스, kind) 스트림에 봉인한다(`Bytes::sunk`). 차례는 깊이 우선(공식 X의 몫 = 내용물 0..n−1의 몫, X의
  남은 스트림), 스레드와 무관하다. 내용물 i는 i − `SEAL_AHEAD`(4)×스레드번째가 봉인된 뒤 `pool::dispatch`로 들인다.
  꽉 찬 블록(`BLOCK`)은 풀의 일꾼에서 끝의 압축과 똑같이 압축하므로 아카이브가 같다. 봉인할 때 앞서 봉인된
  슬라이스와 같거나 그것들로 시작하는 슬라이스는 해시(앞 1 KiB → 길이, XXH3-128)로 맞춰 공유한다.
- **평가기 하나** (`crates/aqr-archive/src/eval.rs`). 값의 그래프를 당기는 쪽이 진행시킨다. 전체형 적용은 처음
  당겨질 때 다 읽어 실행하고, 부분형은 자기 스레드(네이티브: aqr-api 에뮬레이션)나 fiber(wasm)에서 `aqr_request`/
  `aqr_commit`으로 돈다. 출력이 여럿인 생산자(Apply, Split)의 간선은 기다리게 하지 않고 넘치는 바이트를 append
  파일에 둔다(`spool.rs`; 교착은 같은 생산자의 출력을 다른 순서로 읽을 때만 생긴다). 사전 검증, 최종 검증, 해제가
  같은 평가기를 쓴다(`Formula::eval`/`bytes`도 프로그램을 거쳐).
- **검증**(4단계) (`aqr_archive::chash`, `crates/aqr-compress/src/verify.rs`). 이어 붙일 수 있는 내용 해시: CRC-64(NVMe
  다항식)와 길이. 사전 검증은 적용마다 "키(모듈, 매개변수, 리소스의 해시, 입력의 내용 해시) → 출력의 내용 해시
  (입력의 구간이면 그 구간)"만 남기고, 최종 검증은 아카이브 그래프를 아래에서 위로 해시하며 기록이 없는 값만 실제로
  평가한다. 사전 검증 뒤의 변형(`place_all`, 집합, `compact`, `share_slices`, 병합, 봉인의 공유)에도 해시는 구조와
  무관하게 맞는다. 큰 출력의 기록은 프로세스 전체에서 공유한다(`LARGE`).
- **인코더의 힙.** 재압축은 힙 허용량(`aqr_common::allowance`) 안에서 돌고 넘으면 물러난다. 형식마다 입력 1바이트당
  힙의 사전 추정이 `crates/aqr-compress/src/formats.rs`의 `heap`이다(옮긴 코덱은 작다; 지금도 다수가 7–12, bvx 36).
- **출력에 바로 쓰기**(아래 절): 1 GiB 이상의 텐서 파일 하나.

## 제약과 근거

- **parcel 순서.** formula 블록이 non-leaf 블록보다 앞이고(명세 "Block order") 모든 슬라이스의 길이를 먼저
  선언하므로, 공식이 끝나기 전에는 블록을 제자리에 쓸 수 없다. 그래서 싱크의 블록은 스필에서 기다리거나, 아래
  "출력에 바로 쓰기"처럼 formula 자리를 예약한다. 다중 leaf(parcel을 끊기)는 부분끼리 스트림을 못 나눠 쓰지 않는다.
- **결정성.** 싱크에 붙는 순서가 아카이브를 정하므로 봉인 순서는 색인 순이고 `make_later`도 색인 순으로 나눠 준다.
  `fit`(기술 크기 상한)은 슬롯 순서로 누적해 판단한다. 집합(PNG)의 구성원과 `fit`을 넘긴 내용물은 봉인하지 않고
  부모의 남은 스트림으로 간다. 최상위 공식의 남은 스트림은 끝에서 클래스로 합친다.
- **기다리는 스레드는 일꾼을 쥐지 않는다.** 봉인 차례를 `par_map`의 항목 안에서 기다리면 차례를 쥔 긴 내용물이
  일꾼을 못 얻어 10분 넘게 걸렸다. `dispatch`는 들이는 스레드만 기다린다(대가: 그 스레드는 내용물을 만들지 않는다).
- **세그먼트를 스트림 하나에.** 적용을 세그먼트로 나누는 코덱(wrt, fasta, xsv, recs)은 세그먼트들의 부분을 한
  스트림에 이어 두고 적용마다 구간을 받는다. 세그먼트마다 스트림을 두면 싱크의 블록이 작아져 나빠진다(enwik8 +
  enwik9 -9: 222.06 → 224.61 MB).
- **스필 위치.** 출력과 같은 디렉터리(임시 디렉터리는 같은 드라이브라는 보장이 없다).
- **압축 레벨.** `plan`이 블록마다 레벨을 고르면 결과가 끝에서 합친 것과 달라질 수 있다. 받아들인다.

## 측정 (최종)

- Windows Server 2008 R2 DVD(3.17 GB, 안의 WIM을 풀면 약 8 GB, 16스레드): 처음 최대 commit 22.9 GB(힙 26.5 GB까지).
  봉인과 창까지 마친 뒤(Windows) 2,260,604,062 B, 스필 최대 4.45 GB, resident 12.69 GB. 4단계(해시 검증) 뒤(M4):
  힙 최대 2.17 GB, peak footprint 2.23 GB, resident 8.07 GB, 스필 2.51 GB, 검증은 모듈 없이 약 5초. 봉인의 공유는
  `share_slices`보다 +18.0 MB(+0.8%)이다(아래 "남은 것").
- 10gb(M4, 79,431 파일, `-9`, 한도 256M): 5단계 3번(wrt) 전 2,880,101,336 B / 938 s(한도 밖 포기가 많음) → 5단계 끝
  **2,345,160,967 B** / 410 s, 한도 밖 포기 없음, `-K` 통과(45.0 s, resident 3.70 GB). 한도 2G의 2,345,760,077 B보다 작다.
- 49.9 GB 텐서 샤드: 80 GB에서 멈추던 것이 188 s, resident 1.22 GB(아래 "출력에 바로 쓰기").

## 5단계: 코덱

큰 입력을 받는 코덱을 하나씩 쓰는 대로 내보내게 하고, 필요하면 모듈을 part module로 옮겼다. 옮기지 않은 코덱은
`heap` 추정으로 너무 큰 입력의 재압축을 건너뛴다. 분류: A 손보면 스트리밍, B 형식의 단위로 나눈 독립 적용, C 상한만
(jpeg progressive, pdf, pdb, dex, arsc, wasm, gettext, psd, fits, hdf5, gif, tex, clip, ogg: 전역 구조가 있거나 커지는 일이
드물다). 모듈별 계획은 `docs/arena-design.md`, 형식은 각 코덱의 FORMAT.md.

| 번호 | 코덱 | 한 것 | 결과(한도 256M) |
|---|---|---|---|
| 1 | flate | 파싱이 32 KiB 기록 + 1 MiB 창만 쥐고 `store::Writer`로; 세그먼트를 1 MiB 배수에서 앞뒤 256 KiB를 더 두고 읽어 토큰이 같다; `flate-set` part | enwik9.gz 힙 1.98 → 1.54 GB, 아카이브 같음 |
| 2 | zip | 항목을 하나씩; 큰 항목은 1 | |
| 3 | wrt | 32 MiB 세그먼트마다 적용, 사전은 글 전체로 센 것 하나(코딩된 글이 한 적용과 바이트까지 같다) | enwik9 +414 B; 적용 약 105 MiB |
| 4 | fasta | fasta-recons 3 part module(조각 여섯 + 모델, 3.4 MiB); `SEGMENT` 256 MiB는 인코더 몫 | chr1 62.7 MB(포기) → 51.73 MB |
| 5 | flate-set, bundle, raster | flate-set 4: `BIG`(256K 토큰) 이상 블록은 빈도를 머리에 두고 토큰을 흘림, CRC는 index에; bundle-recons 2 part; raster 48 MiB 띠(`raster::BAND`) | text8.zip 351 → 103 MiB; 126 MB BMP +0.006% |
| 6 | lzma | 아래 절 | |
| 7 | mszip, bcj2, bcj | 셋 모두 part(bcj2는 출력 셋, aqr-7z는 `Formula::share`) | CAB 88.6 → 53.1 MB, bcj2 7z 95.1%, bcj `.xz` 90.6% |
| 8 | pcm, flac | part, 인스턴스가 첫 단계와 오차 단계, gang(LMS + 일꾼 일곱) | 300 MB WAV 2224 → 18.4 MiB 선언 |
| 9 | bz2 | part, 블록을 RLE1로 쌓아 압축해 흘림 | rlib.bz2 51.2 → 29.6 MB |
| 10 | lz4 | part, 리터럴 `LIT_MAX` 4 MiB(넘는 스트림은 인코더가 쓰지 않음) | mozlz4 55% → 39.5% |
| 11 | snappy | part(조각이 독립) | 쓰는 입력 없음(LevelDB는 `leveldb-recons`, whole) |
| 12 | xsv, recs | xsv 64 MiB 세그먼트(`parse_part`), recs 32 MiB/4 Mi 행 묶음; 모듈 그대로 | 209 MB CSV +0.10%, 적용 632 → 204 MiB |
| 13 | git | git-pack(-index)-recons 2 part; 입력을 Split으로 `streams`·`table`, 태그 뒤에 필드 | 인덱스 계획은 항목당 36 B(184.9 → 4.85 MB) |
| 14 | exe x86, arm64 | part(`decode_core`), 형식 그대로 | msedge.dll 36.5% → 27.1% |
| 15 | png-recons | 판 5 part; 머리/rANS/raw로 입력을 가름; 8 MiB 이하 이미지는 8장씩 태스크 | 100 MP PNG, APNG 포기 → 87%, 82% |
| 16 | macho-codesign | 판 2 part(서명은 해시하는 바이트보다 뒤) | Electron Framework 67.7 → 55.8 MB |
| 17 | mp3-recons | 판 6 part(프레임 큐 1024, 저수지 창 512 KiB) | 248 MB MP3 포기 → 88.1% |

### 5단계 6번: lzma

- 모듈의 파인더 표는 해시마다 위치 하나(12.5 MiB; 네이티브는 16개), 후보와 기록은 같다.
- `lzma-recons` 4, `lzma2-recons` 3은 part module이다: 데이터 창은 사전만큼 뒤 + `LOOK` 66 KiB, 레코드 앞의 목록은
  본문 안으로(LZMA2 청크 목록은 청크 자리에서 적응 비트로). 계획 ≈ 12·링 + 12.5 MiB + 데이터 창.
- **도달 거리 R**(레코드 머리 1바이트: 0은 사전, 아니면 log2 R). 파인더와 체인의 링만 R로 줄이고 데이터 창과 거리
  한계는 사전 그대로라 R 밖의 매치·literal·rep은 기록에 더 싣지 않는다(`far` 경로). 인코더는 `--application-bound`
  안에 드는 가장 큰 2의 거듭제곱을 고른다(`plan::finder_for`; 한도는 `plan::set_bound`로 프로세스에 건다). 7z의
  64 MiB 사전은 R = 8 MiB, 계획 186 MiB. R별 기록 크기(쪼개기만, MB):

  | 스트림 | 무제한 | R=16 | R=8 | R=4 |
  |---|---|---|---|---|
  | 7z 폴더 0 (249 MB, 64 MiB) | 4.308 | 4.799 | 5.389 | 5.979 |
  | 7z 폴더 1 (113 MB, 64 MiB) | 1.750 | 1.950 | 2.211 | 2.387 |

  결과: `mingw480`(7z와 트리 409 MB) 78.9 MB(7z 포기) → 50.1 MB(한도 2G의 48.6 MB에 +1.54 MB). 10gb 전체 −16.61 MB.
- **버림.** LDM 색인(R 밖 후보를 성긴 해시로): R = 8에서 순이득 17.0 MB(R만 16.6 MB)인데 계획 234 MiB, 쪼개기 50–75%
  느림. 창 자체를 R로 자르는 안: 먼 literal의 맞춘 바이트와 먼 rep의 길이를 기록에 실어야 해 더 크다.
- **비용.** 네이티브 쪼개기·다시 만들기 +4.7% 명령; 알려진 해시의 복호는 part 경로라 한 스트림이 한 스레드다(네이티브
  `Run::Threaded`는 그대로 lzma를 통째로 스레드로 돌린다).

## 출력에 바로 쓰기: formula 자리의 spacer

봉인과 싱크로도 스필은 아카이브만큼 남는다(블록이 압축된 채 formula를 기다린다). 거의 줄지 않는 큰 파일에서는 입력에
가깝다. 그래서 큰 입력은 formula가 올 자리에 그 상한보다 조금 큰 spacer를 먼저 쓰고, 싱크의 블록을 채워진 차례대로
출력에 바로 쓴다(`Sink::direct`; 대기 블록은 스레드 수까지). 끝에 그 자리를 모듈·리소스·formula 블록으로 덮고 남은
바이트는 spacer다(최소 2 B; 0으로 채운 자리는 2 B spacer의 연속이라 쓰는 동안에도 구조가 맞다). formula는 non-leaf
블록을 ordinal로 가리키므로 블록이 클래스마다 섞여도 된다.

```
[HEADER] [spacer: 예약 R] [non-leaf 블록 … 채워지는 차례대로] [footer]
            ↓ 끝에서
         [모듈][formula][spacer: R − 앞의 것]
```

- 구현: `direct_room`(`crates/aqr-compress/src/output.rs`; `DIRECT` = 1 GiB, 입력만의 함수), aqr-tensors의
  `formula_bound`/`formula_bound_layout`과 `encode_sealing`(세그먼트를 코딩한 일꾼이 부분 공식의 한도 선언, 사전
  검증, 블록 압축까지; 차례 스레드는 붙이기와 쓰기만), `Encoder::direct_head`. GGUF, `.pth`, safetensors, 힌트로
  텐서라 한 머리 없는 파일(`db1ea2cb`). `-P`도 된다(`Sealer::place`, `5965ae36`). 넘치면 쓴 블록을 뒤로 민다
  (`moved_back`). 검증이 실패하거나 재압축이 도중에 포기해 참조되지 않는 블록이 생기면 출력을 비우고 raw로 쓴다.
- 결과(Ryzen 7 9700X, 16스레드): Q4_K_M 5.34 GB 18.65 → 13.1–13.5 s, 스필 5.66 GB → 15.8 MB, resident 15.89 → 1.26 GB,
  아카이브 +299 KB(남은 예약, 블록 순서). 49.86 GB 샤드 188 s, resident 1.22 GB, 47.72 GB.
- 안 되는 곳: stdout·파이프(위치를 못 정한다), `-A`(미러 블록과의 배치를 정하지 않았다), 트리 아카이브(7단계), 상한을
  내지 않는 다른 재압축기.

## 남은 것

- **7단계: 트리 아카이브의 배치.** 레벨 2 이상에서 배치는 아카이브로 스토어(스필)에 쓰인 뒤 출력에 복사된다
  (`3f1b1f44`; 검증이 배치를 따로 디코딩하므로). 배치마다 formula 자리를 예약해 leaf를 출력에 바로 쓰면 배치 크기만큼의
  스필이 없어진다. 배치의 `share_slices`를 해시 기반으로(봉인의 공유처럼) 하는 것도 여기다. 수십 GB 파일이 여러 파일의
  아카이브에 들 때 지금은 배치를 통째로 쥔다(`codecs/aqr-tensors/README.md` Left).
- **6단계: 우회 정리.** `spill.rs`의 쓰기 가능한 매핑 경로, `Bytes::lazy`(aqr-wim의 데이터), `store::Buffer`, 다 만든
  공식의 봉인. 해시 검증이 늘 끝까지 가므로 리프를 통째로 푸는 길(`verify::Cached`, `each_leaf_only`)은 오류 때만 쓰인다.
  (`each_leaf_only`는 aqr의 serve, edit, convert도 쓴다.)
- **봉인의 공유 손실(+0.8%, DVD).** 앞 것의 앞부분과 같거나 앞부분 절반 이상을 공유하는 슬라이스는 앞 것의 바이트가
  압축된 블록에 있어 찾지 않는다. 압축된 블록을 풀어 비교하면 되찾는다(비용은 그만큼의 해제).
- **해제.** 블록을 통째로 푸는 unzstd(블록은 `BLOCK` 이하)가 `-K`의 힙 최대; 큰 leaf; 호스트가 part module의 입력을
  슬롯(leaf)으로 쥐는 것(mp3 248 MB에서 470 MB); `-K`가 아카이브 매핑을 놓지 않는 것(49.9 GB에서 resident 48 GB;
  `let_go_read`는 압축의 검증과 convert만 쓴다).
- **네이티브 `Run::Threaded`**(lzma, lzx, bz2, png, mp3, flac, pcm, mca)는 통째로 스레드에서 돌아 계획을 따르지 않는다
  (한도만 확인).
- **whole로 남은 모듈.** x86-remap(참조를 모아 값으로 정렬: 형식을 바꿔야 한다), pe-reloc(작다), arm64-remap,
  arm64-delta, leveldb-recons, png-against·png-set(부모의 참조를 쥔다).
- **앞 단계가 출력을 통째로.** git의 flate-set 출력, macho-codesign 본문을 만드는 arm64 필터·심볼 표.
- **코덱별.**
  - flate: 적응형 프로파일(레벨 7 이상)의 해체가 토큰을 6 B씩 쥔다(enwik9.zip 1.13 GB, 파싱 중 `raw.len()`개를 한꺼번에
    잡음). 블록의 비트 위치를 기록하면 토큰 없는 `Flat`이 블록마다 다시 파싱할 수 있다. flate-set의 바닥은 `heads` 표
    (33.5 MB)와 `pull_room`(18 MB). 부분형 계획은 `plans_hold_on_samples`가 확인하지 못한다.
  - wrt: Wasm 해제가 적용마다 사전을 다시 파싱한다(세그먼트 30개에서 +4%); 64 MiB 세그먼트는 계획 약 200 MiB로 빠듯;
    사전 세기의 메모리는 서로 다른 토큰 수에 비례.
  - lzma: 사전 32 MiB 이상의 스트림은 R로만 한도에 든다(R을 고르지 않으면 16 MiB까지). `stream.rs`(`Pull`·`Reader`)가
    aqr-fasta의 것과 거의 같아 aqr-common으로 합칠 수 있다(fasta 모듈이 다시 만들어진다).
  - pcm, flac: 오차 단계를 태스크로 빼려면 입력 창을 태스크가 볼 수 있어야 한다(명세의 Issue). 읽는 창(프레임 최대의
    두 배)을 실제 양에 맞추려면 프레임 중간에 채우는 읽개가 필요하다.
  - xsv: 인코더의 필드 벡터(필드당 24 B); xsv·recs의 계획 시험(`applications_hold`)이 없다.
  - git: id를 쥐지 않으려면 id를 인덱스 순서로 두고 순열을 저장해야 한다(항목당 약 3 B 더); 모듈 +7 KB.
  - mp3: 512 MiB 상한(저수지 위치가 32비트 비트 수), payload 없는 free format 프레임 1024개 이상.
  - arm64 exe, snappy: 실제 입력으로 잰 아카이브 수치가 없다.

## 버린 것

- 쓰기 가능한 매핑의 64 GiB sparse 세그먼트(길이를 모르는 스트림용): Windows에서 매핑이 그 길이만큼 commit을 잡아
  수백 개에서 바닥났다. `store::Writer`의 4배 영역으로 바꿨다.
- 최종 검증의 (b) 전체 재해제: 해제 CPU가 두 배. 기저가 무작위인 다항식 해시(2^61 − 1): 2.9 GB/s(CRC-64는 50–60 GB/s).
- aqr-wim 리소스마다 스트림(파일마다 내용물): −134 MB(−6%)에 +46 s, 따로 판단할 일로 남김. 붙은 무압축 리소스를
  나누기만 해도 +3.9 MB.
- 블록을 봉인하는 스레드에서 압축: 끝 10초를 일꾼 하나가 했다. aqr-tensors의 한도 선언·사전 검증·블록 압축을 차례
  스레드에서: 5.3 GB가 18.65 → 24.04 s.
- fasta 세그먼트 64 MiB(+0.64%)와 96 MiB(+0.45%, 무작위 염기의 계획이 한도에 8%만 남음): part module(판 3)로 대신했다.
- lz4 `LIT_MAX` 256 KiB: rlib의 mozlz4가 넘었다.

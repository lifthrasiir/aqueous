# 모듈 메모리: 상한과 계획

모듈(Wasm, 복원 쪽)이 적용 하나에 쓰는 working memory를 입력에서 계산되는 값(계획)으로 묶고, 그 값을 Apply의 메모리
한도로 선언하는 설계. 메모리 모델(메모리의 종류, 한도의 계산, 태스크의 영역)은 spec의 "Module execution"과 "Tasks"가
정한다. 처음의 추정 함수와 bump 아레나는 버렸고(추정 API `memory_usage_in_pages`, `with_memory`, `Estimate`, "추정 × 5/4 +
1 MiB"는 `c9c475a0`에서 없어졌다), 수명을 LIFO로 맞춘 스택 배치와 정확한 계획이 됐다. 끝낸 일의 경과는 git 이력에 있다.

## 현황

- **모든 모듈이 정확한 계획을 가진다.** `Module::new(..).with_plan`(태스크를 spawn하면 `with_task_plan`)이 인스턴스 한도와
  클래스마다의 태스크 한도를 입력의 길이·params·리소스에서 계산한다. 코덱의 `plan` 함수(`aqr_api::stack::Sim`으로 Wasm의
  할당을 따라간다)가 디코더의 배치와 인코더의 선언의 같은 출처다. 여유 없이 페이지로 올림만 한다. `*_plans_hold`
  (aqr-decompress `tests/wasm.rs`)가 실측 = 계획을 페이지까지 보고, `plans_hold_on_samples`(`tests/samples.rs`, ignored)가
  `scripts/module-memory.sh`의 표본을 본다. `crates/aqr-decompress/examples/memory.rs`는 모듈별 최대치와 계획을 견준다.
- **Apply는 항상 opcode 16**(인스턴스 한도 하나와 클래스마다의 태스크 한도). opcode 11은 릴리스된 모듈에만 받고 한도를
  강제하지 않는다(`crates/aqr-archive/src/released.rs`).
- **적용마다 상한.** 적용의 최소 한도(인스턴스 + 태스크 한도의 합, 동시성 1)가 인코더는 256 MiB
  (`aqr_compress::memory::DEFAULT_BOUND`)를 넘으면 그 적용을 쓰지 않고, 디코더는 512 MiB(`aqr_formula::SUPPORTED_BOUND`)까지
  지원한다(`--application-bound`).
- **네이티브도 같은 한도.** 알려진 해시로 네이티브에서 도는 모듈도 실행 전에 계획(`aqr_decompress::needed_by`; 인코더의
  `memory::limit`이 같은 함수를 부른다)을 Apply의 한도에 대 보고 넘으면 자원 실패다. 계획이 내용을 읽는 모듈(flate-set의
  레코드와 색인 등)은 입력이 한도 이하일 때만 통째로 당겨 계산한다. 계획을 못 구하면 Wasm으로 돌려 거기서 강제한다.
  `aqr-native`의 `Run`: lzma, lzma2, bz2, bz2-alz, png, jpeg-lossless, mca, mp3, flac, pcm은 네이티브에서 통째로 스레드에 올려
  돈다(`Run::Threaded`: 한도는 확인하지만 실행이 계획을 따르지 않는다). lzx는 길이에 따라 통째 또는 에뮬레이션의 스트리밍
  (`Run::Streamed`), prores 등은 에뮬레이션(`Run::Part`).
- **인코더의 재압축은 힙 허용량 안에서 돈다**(`aqr_common::allowance`; 한도는 `-M`, 없으면 max(예산, 전체/2); 넘으면 그 내용을
  그대로 둔다). 형식마다의 사전 추정은 `crates/aqr-compress/src/formats.rs`의 `heap`이다. 디코더의 모듈 메모리와는 별개다.
- **part 모듈**(`entry! … windows(…)`, 계획이 입력·출력 길이와 무관하고 사전·창·params의 최대치만 따른다): ac3, b2t, bcj 계열
  (x86, arm, arm64, armthumb, ia64, powerpc, riscv, sparc, swap2/4, delta), bcj2(출력 셋), bundle, bz2, bz2-alz, dh-lods, div3,
  exe-recons(x86, arm64, macho-codesign), fasta, flac, flate-inflate, flate-set, git-pack, git-pack-index, jpeg-lossless, lz4,
  lzma, lzma2, lzx, mca, mp3, mszip, pcm, pdb-types, png-recons, prores, snappy, tensors, zip-recons, zstd-frames. 나머지는 whole
  (`crates/aqr-modules/src/bin`에서 `part fn`이 없는 것).

## 계약

- 인코더는 계획을 Apply의 한도로 선언한다. 디코더가 그만큼 주지 못하면 시작 전에 resource failure이고, 시작했으면 한도 안의
  `memory.grow`는 성공한다(spec "Module execution"). 모듈이 계획을 어기면(구멍, 순서를 어긴 해제) 한도에서 막혀 실패한다.
  그래서 모듈은 계획만큼의 메모리가 늘 있다고 가정한다. 단위는 페이지(64 KiB).
- 클래스마다 동시에 도는 태스크는 최대 그 클래스의 slots개다(spec "Tasks"; `aqr_task_slots`, CLAUDE.md의 Modules 절). 호스트의
  스레드 수는 상한에 들지 않는다. 디코더는 동시성을 1부터 slots 사이에서 고른다(`choose_concurrency`, `-M`이면 낮춘다).
  파이프라인은 단계가 slots를 넘으면 교착(잘못된 아카이브)이다.

## 스택 배치의 규칙

할당과 해제의 순서를 미리 정해 두면 최대치는 재지 않고 계산된다. 수명이 LIFO이면 영역은 스택이다.

- `aqr_api::stack::Stack`(모듈 바이너리의 `#[global_allocator]`): 블록을 맨 위에 쌓고 해제되면 내리며, 맨 위 블록은 제자리에서
  자라거나 준다(크기를 모르는 마지막 영역). 순서를 어긴 해제는 구멍이고, 구멍이 64개(`HOLES`)를 넘으면 트랩한다. dlmalloc이
  빠져 모듈도 작다(lz4 18,835 → 10,786 B, zip 30,473 → 25,880 B). `#[global_allocator]`를 빠뜨린 바이너리는 기본 할당자라
  계획이 맞지 않는다(clip-recons에서 한 번 그랬다).
- 고쳐 온 것의 유형(모듈마다 되풀이됐다):
  - 개수를 아는 목록은 정확한 용량으로 한 번 잡는다(`collect`, `push`, `Vec` 성장 정책에 기대지 않는다; std의 제자리
    `into_iter().map().collect()`도 보장이 아니다). 개수는 입력을 한 번 훑어 센다(레이아웃, 색인의 varint 수 등).
  - `Vec<Vec<_>>`와 HashMap/BTreeMap은 평평한 버퍼(누적 합 + 원소)와 열린 주소 표로 펼친다. 앞에서부터 놓이는 벡터의 벡터는
    항목마다 구멍을 남겨 64개를 넘는다(실제 PDB의 pdb-msf, 실제 Mach-O의 macho-symtab, 청크마다 표를 다시 만들던 psd·raster).
  - 디코딩 전에는 알 수 없는 개수(토큰·DIE·스코프·키 목록·블록 크기 등)는 encoder가 params나 레코드에 적고(형식 변경),
    decoder는 그만큼 잡고 넘거나 모자라면 실패한다. 디코더가 따로 세면 비싼 경우가 그렇다(아래 측정).
  - 데이터를 따르는 작은 작업(허프먼 표 짓기 등)은 스트림마다 `SCRATCH`를 한 번 잡았다 놓아 최고점을 고정한다(lha 256 KiB,
    rar5 64 KiB, lzx 128 KiB: 모든 할당과 구멍의 합보다 크게).
  - 구조체의 벡터는 만든 순서의 거꾸로 선언해 거꾸로 놓이게 한다(`psd::Lifo`, vorbis, mp3, sqlite의 `Drop`). Wasm에서는 끝에
    디코드의 것을 놓지 않는다(출력 아래의 구멍이 될 뿐이다).
  - 출력은 정확한 길이로 먼저 잡거나, 태스크가 있으면 spawn 뒤에 잡는다(태스크가 복사하지 않게). 결과를 이어 쓰는 쓰개는
    정해진 여유(`TOKEN_SLACK`)로 잡아 자라지 않게 한다(`compress_onto`, `reconstruct_onto`).
  - 처음 쓸 때 잡히는 `OnceLock` 표(Wasm의 CRC-32 16 KiB, 고정 허프만 부호)는 처음에 만들거나 계획에 넣는다.
  - 계획이 Wasm 구조체 크기를 쓰면 `module::wasm`/`plan::wasm`에 상수로 두고 Wasm 빌드에서 `size_of`와 assert한다.
  - `&mut Vec<u8>`에 이어 쓰는 싱크 같은 곳은 제네릭이 아닌 `dyn`(모듈 크기, CLAUDE.md의 단형화 원칙).
- 측정은 계획이 맞는지 보는 테스트로만 쓴다.

## 태스크의 계획과 검증

태스크는 spawn이 주는 영역(spec "Tasks"의 region records)에서 시작한다. aqr-api의 spawn은 늘 working memory 전체를 영역
하나로 준다(`aqr_api::task::spawn_record`). 그래서 태스크 하나 = spawn 때의 인스턴스 메모리 + 제 몫이고, 계획은 spawn마다
(결과를 받을 때의 인스턴스 + 큰 것부터 slots개의 태스크)와 인스턴스의 끝 중 큰 것이다. gang의 한도는 태스크들의 합 + 채널이
담을 수 있는 메시지다. spawn 레코드는 인라인되지 않는 프레임에 둔다(`spawn_record`; 아니면 태스크의 스택이 호출자의 지역 변수를
덮는다).

검증: 엔진(`aqr-wasmrt`)이 적용마다 인스턴스의 최대치, 클래스마다 한 단위가 한꺼번에 쥔 최대치와 끝난 태스크들의 합
(`task_memories`), 고른 동시성을 남기고(`wasm.rs`의 적용 기록), `plan_holds`/`plan_holds_queued`/`plan_holds_bounded`가 계획과
페이지까지 견준다(gang은 채널에 쌓일 수 있는 메시지 몫을 빼고; `bounded`는 동시성 1에서도 맞는지).

## 측정과 버린 것

계획의 근거가 된 수치와, 다시 시도하지 않을 것.

| 모듈 | 결과 |
|---|---|
| zip-recons | 다시 쓰이는 deflate 본체의 길이 합(`again`)을 레이아웃 머리에 적었다(version 5): zip.zip 17 MB의 working memory 19.2 → 2.29 MB |
| png-set | 부모의 참조를 블록별 토큰 수(`ntok`)로 한 번에 잡아 set.tar 678 → 232 MB(`f591e33`). Wasm에서는 자식을 만드는 태스크가 출력 속 부모 파일로 참조를 다시 만든다(인스턴스가 쥐면 깊이마다 구멍): 세트의 Wasm 디코드 약 7% 느림 |
| dwarf-info | 문자열·DIE·열린 DIE·전방 참조 수를 디코더가 세면 Wasm 명령어 +5%(aqr-rustc.dSYM) → params. 전방 참조를 REF의 varint 수로 묶었더니 72 MB를 예약했고, 건드리지 않은 예약도 wasmtime에서 모듈 시간의 약 5%였다 |
| bz2 | SA-IS의 형을 비트로 두면 명령어 +14% → 바이트 |
| git-pack-index | 항목 19157개에서 계획 184.9 → 4.85 MB(part; 항목당 36 B) |
| prores 3 | 1080p 4:2:2 HQ의 64 MiB 덩어리 404 → 70 MB, UHD 4:4:4 410 → 220 MB |
| flate-set 3, 4 | ZipBefore의 CRC-32를 index에 먼저 적어(`index_before`) enwik9.zip 항목 353 → 108 MiB. `BIG`(256K 토큰) 이상 블록의 기호 빈도를 헤더 앞에 넣어 text8.zip 351 → 103 MiB(아카이브 +3.5 KB) |
| exe-recons | part로: x86 357 MB에 1240 MiB, macho-codesign 196.7 MB에 753 MiB였던 것이 길이와 무관 |
| pcm/flac | part로: 300 MB WAV 2224 MiB, 162 MB FLAC 1965 MiB였던 것이 길이와 무관 |
| fasta 3 | part로 3.4 MiB(한도 최대 9.5 MiB). 메모리 때문에 자를 이유가 없어 `formula::SEGMENT`를 256 MiB로 |
| lzma | 모듈의 파인더 표를 해시마다 위치 하나로(12.5 MiB, 132.5에서): 기록은 같고 Wasm에서 2배 느리다(네이티브와 인코더는 16개 그대로). 파인더의 도달 거리 R(레코드 머리의 바이트, `plan::finder_for`)로 7z의 64 MiB 사전이 R = 8 MiB에 계획 858 → 186 MiB |
| jpeg | encoder가 줄무늬·스캔의 비트를 재느라 압축이 늘었다. 그대로 재현되는 스캔은 destuff 길이로 바꿔 대부분 없앴다 |
| bcn | 태스크가 인스턴스 전부를 복사해 2048² BC3에서 24 MB × 8 = 244 MB(옛 추정 210 MB보다 큼) |
| png 5 | APNG의 쓰기 태스크는 두지 않았다(한 장은 같은 속도) |
| mszip 2 | 합친 스트림의 블록이 96 KiB를 넘게 걸치면 거절(CAB에서는 나오지 않는다) |
| lz4 2 | 한 시퀀스의 리터럴은 `LIT_MAX` = 4 MiB까지. 넘는 스트림은 인코더가 `part::reproduces`로 걸러 그대로 저장 |
| mp3 6 | 새 거부: 큐 1024개 초과, 비트열 끝 너머 읽기, 512 MiB 이상 파일. 어떤 부호화기의 출력도 걸리지 않는다 |

모듈별 계획의 모양은 각 코덱의 `plan.rs`/`module.rs`와 FORMAT.md(params에 넣은 개수)에 있다.

## 남은 것

- 네이티브의 `Run::Threaded`(위 목록)를 계획을 따르는 part 실행으로 옮긴다. 지금은 한도 확인만 한다.
- 사전이 아주 큰 lzma: 데이터 창이 사전을 따르므로 200 MiB 남짓을 넘는 사전은 R을 줄여도 256 MiB에 들지 않는다.
- 태스크의 사본: jpeg(줄무늬의 계수 버퍼), fits-rice, bcn, flate-set, png-set은 태스크마다 인스턴스 전부를 복사한다. spec은
  영역을 골라 넘기는 spawn을 이미 허용하므로(region records), aqr-api에 영역을 고르는 spawn을 두면 줄일 수 있다.
- png-set의 참조 자체(원시 바이트마다 약 10 B)와 자식 태스크마다의 복사. 참조를 매치만 적는 꼴로 줄이려면 리터럴이 참조
  데이터의 바이트를 요구하는지 먼저 확인해야 한다.
- flate-set의 바닥: `heads` 표(8 B 슬롯, 33.5 MB; u32로 줄이려면 데이터를 읽어 키를 확인해야 해 인코더의 탐색도 느려진다)와
  `pull_room`(배치 둘의 데이터, 18 MB).
- 입력을 쥐는 나머지 whole 모듈(exe-recons의 x86-remap, pe-reloc, arm64-remap, arm64-delta, leveldb-recons, jpeg-recons 등)의 part
  판은 길이가 한도에 걸리는 것부터. 인코더 쪽의 진행은 `streaming-encoder-design.md`.

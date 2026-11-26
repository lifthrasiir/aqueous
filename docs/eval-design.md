# formula 평가기의 메모리

아카이브 디코더가 formula를 평가하면서 쓰는 메모리를 다룬다. 평가기는 `crates/aqr-archive/src/eval.rs`(모듈 문서가 동작을
적는다)이고, aqr-decompress의 해제와 aqr-compress의 검증(`crates/aqr-compress/src/verify.rs`)이 함께 쓴다. 2026-10-23의
초안이었고 제안은 대부분 다른 모양으로 구현되었다. 끝낸 일의 경과는 git 이력에 있다.

절과 항목의 상태 표기는 `xform-design.md`와 같다.

관련:

- `spec/index.bs` §Formula evaluation: 스택의 값은 완전히 평가된 바이트열이고, 지연·스트리밍·부분 평가는 결과를 바꾸지
  않는 구현 전략이다. 이 문서는 그 전략만 다룬다.
- `spec/index.bs` §Module execution: 전체형과 부분형, 메모리 한도, minimal bound(디코더는 512 MiB까지 지원해야 한다).
- `xform-design.md` §4.3(평가 의미론), §5.3(데이터 전달과 구현 노트), §5.4(요구 구동 스케줄링).
- `docs/arena-design.md`: 모듈의 작업 메모리 계획.

---

## 0. 전제

- **wasm 선형 메모리는 줄지 않는다.** 할당기의 최고 수위가 그대로 점유가 되고, 범용 할당기에서는 단편화로 더 오른다.
  모듈의 작업 메모리를 정확한 LIFO 계획으로 두는 이유이다(`docs/arena-design.md`).
- **크기는 모두 미리 알려져 있다.** 블록, literal, `apply`가 선언한 출력, 그 합인 `concat`. formula 블록은 평가 전에 모두
  읽힌다.
- **값은 정확히 한 번 쓰인다(명세).** formula 하나는 명령의 트리이고, 갈래가 생기는 곳은 출력이 여럿인 Apply와 Split뿐이다.
  non-leaf 블록만 여러 slice와 formula에 걸쳐 공유된다.

## 1. 구현된 것 [구현]

- **스트림 평가와 싱크.** 값의 그래프에서 출력을 조각씩 당긴다. `concat`과 `pack`은 만들지 않고 항목을 차례로 당기며,
  slice는 블록의 바이트를 공유한다. 최종 출력은 싱크로 나간다(`Archive::write_file`, `each_leaf`).
- **부분형 모듈**은 자기 스레드에서 입력을 당기고(`Pulled`) 출력을 큐에 민다(`eval:queue` 16 MiB). 네이티브로도 부분형이
  돈다(aqr-native의 `Run::Part`, `Run::Streamed`). lzma, lzx, bz2, png, mp3, flac, pcm, mca는 네이티브에서 통째로
  스레드에 올린다(`Run::Threaded`).
- **상한들.** 차례 밖에서 당겨진 출력은 `eval:held`(64 MiB)를 넘으면 파일로 내보낸다(`spool.rs`). 미리 만드는 조각은
  `eval:ahead-part-bytes`(256 MiB), 풀어 둔 블록의 캐시는 `eval:fresh`(512 MiB)까지이다.
- **검증**은 출력을 원본과 비교하는 싱크가 아니라 내용 해시로 한다. 미리 평가한 적용은 입력 해시의 기록으로 확인하고
  모듈을 다시 돌리지 않는다(`verify.rs`, `aqr_archive::chash`).
- **메모리 한도.** 모든 모듈이 정확한 계획(`with_plan`, `aqr_api::stack`)을 가지고 Apply가 그 한도를 싣는다. 실행 전 거절은
  명세의 minimal bound가 정한다(초안의 열린 질문 3). 일반 오프셋 계획기(TFLite식 greedy 배정)는 쓰지 않았다.

## 2. 남은 것

- **평가기 버퍼의 정적 계획 [제안].** 평가기 자신의 큰 버퍼(통째로 푼 블록, 전체형 적용의 입력과 출력)의 최대 동시 점유를
  실행 전에 계산하지 않는다. 지금은 1절의 상한으로 묶일 뿐이다. 계획이 있으면 블록의 수명(첫 slice부터 마지막 slice까지)과
  트리 순회의 LIFO 수명으로 최대치를 미리 내고, 그 한도를 넘는 아카이브를 실행 전에 자원 실패로 돌릴 수 있다.
- **블록을 소비자에게 바로 풀기 [제안].** 블록은 첫 slice 앞에서 통째로 풀린다(`blocks_ahead`). slice들이 평가 순서대로
  오프셋이 늘며 한 번씩만 읽히는 블록은 zstd 스트리밍 해제로 소비자에게 바로 풀고 사이의 바이트는 버릴 수 있다. 동기였던
  예: VS Code macOS 빌드(266 MB)의 ZIP 간격 블록 695 MB가 통째로 풀려 해제의 최대(2.00 GB)를 키웠다(2026-10-23, 그 뒤 ZIP
  모듈이 부분형이 되어 그 최대는 다른 모양이다).
- **전체형 네이티브 모듈의 출력 버퍼.** `aqr_api::dll::NativeDecode`는 `Vec`를 돌려준다. 호스트가 선언 크기의 버퍼를 주는
  형태(`out: &mut [u8]`)여야 정적 계획에 들어간다.
- **재압축 중의 최대**는 이 문서의 범위 밖이다(aqr-zip 인코드 안의 aqr-flate 분해: 155 MB 스트림에 약 0.8 GB, 이전 측정).

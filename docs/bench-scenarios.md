# 벤치마크 시나리오

통째로 압축하고 푸는 것(`10gb-todo.md`의 측정) 밖의 쓰임새를 재는 시나리오와 그 결과를 적는다.

## 경로 하나만 풀기 (10gb)

`aqr -d ARCHIVE PATH`(`7603762`)로 10gb 아카이브에서 파일 하나만 풀 때 걸리는 시간이다. 경로가
필요로 하는 leaf만 디코딩하므로, 시간은 그 파일 자신의 재구성과 아카이브를 여는 고정비용으로 나뉜다.

### 방법

- 아카이브: `7603762`로 `cargo aqr -c -f ~/tmp/10gb`(level mid; 2.50 GB, 171 s).
- 표본: 압축의 `--stats=FILE.json`에 적힌 content를 종류(`kind`)와 모듈 조합으로 나누고, 그 안에서
  크기 순으로 고르게(가장 큰 것은 반드시) 뽑았다. 종류마다 몫이 있고(zip 120, 나머지 8–30, 개수가
  적은 7z·tar·xz·ppmd·chm은 전부), 조합마다 개수의 제곱근에 비례해 나눈다. zip은 안에 든 것에 따라
  재구성 시간이 크게 달라질 수 있어 몫을 늘렸다(21개 조합에서 114개). 다른 파일과 같은 것(`same_as`)
  10개를 더해 모두 428개이다.
- 한 번에 하나씩 `aqr -d -o DIR ARCHIVE PATH`를 `/usr/bin/time -l`로 쟀다(bincache의 바이너리를
  직접 실행해 cargo의 시간은 빠진다). M4 10코어, 다른 부하 없음. 모두 성공했다.
- 스크립트와 결과(`samples.tsv`, `results.tsv`, `summary.md`)는 `crates/aqr/cache/select-10gb-7603762/`
  (gitignore)에 있다.

### 결과

1 MB 미만 파일 359개: 중앙값 0.17 s, p90 0.19 s, 최대 0.38 s. 대부분은 아래의 고정비용이다.

| 종류 | 개수 | 중앙값 s | p90 s | 최대 s | 최대 footprint |
|---|---:|---:|---:|---:|---:|
| 7z | 4 | 0.17 | 16.76 | 16.76 | 1.6 GB |
| zip | 114 | 0.18 | 0.20 | 3.76 | 1.9 GB |
| lzma | 25 | 0.24 | 2.22 | 2.84 | 731 MB |
| fasta | 8 | 0.17 | 2.30 | 2.30 | 491 MB |
| text (wrt) | 12 | 0.18 | 0.24 | 1.78 | 1.9 GB |
| gif | 12 | 0.17 | 0.17 | 1.01 | 236 MB |
| tar | 7 | 0.21 | 0.93 | 0.93 | 439 MB |
| pe | 35 | 0.27 | 0.34 | 0.59 | 445 MB |
| 나머지(jpeg, png, pdf, rar, gzip, ar, …) | | 0.17 | ≤ 0.21 | ≤ 0.38 | ≤ 547 MB |

느린 것:

- `mingw480/x32-4.8.0-release-win32-sjlj-rev1.7z`(37.1 MB): **16.8 s**(user 16.6 s). lzma-recons가
  스트림 전체를 한 스레드로 다시 만든다. 아카이브 전체를 푸는 것(38.8 s)의 가장 긴 경로이기도
  하다(`10gb-todo.md` 6번).
- mingw의 `*.tar.lzma`(6–10 MB): 1.9–2.8 s. 역시 lzma-recons이다. 크기에 비해 가장 느린 종류이다.
- `enwik9.zip`(323 MB): 3.76 s(24.8 스레드 초). 같은 내용인 사본(`same_as`)도 같다.
- `silesia.zip`(67.6 MB, 안에 모듈 16가지): 1.96 s.
- `hg19/chr1.fa`(254 MB) 2.30 s, `enwik9`(1 GB) 1.78 s: 크기에 비해 빠르다.

zip은 안의 내용에 따른 차이가 10gb에서는 드러나지 않는다. 표본의 zip은 작고(중앙값 51 KB) 대부분의
조합(flate-set, png, exe, pdf, gif, recs-fields, …)이 중앙값 0.17–0.20 s로 고정비용뿐이다. 0.3 s를
넘는 것은 큰 zip 셋(`picasa.zip` 0.51 s, `silesia.zip`, `enwik9.zip`)이며, 시간은 내용보다 크기를
따른다.

### 고정비용

빈 파일 하나에도 0.17 s, 명령어 3.0 G, footprint 235 MB가 든다(없는 경로의 오류도 0.13 s). 그
가운데 `--stats`가 재는 풀기 자체는 0.03 s이다. samply로 보면 약 75%가 아카이브를 여는
`open_as`이다: 트리 읽기(`tree::read` 39%, 주로 `tree::extras`와 `tree::item`의 CBOR 값 만들기),
`parse` 25%, 메타데이터의 zstd 풀기 13%. 할당(malloc/realloc)이 모두 합해 32%, 이름의 `HashMap`
(SipHash)이 7%, `contents_of`가 12%이다. 엔트리 수에 비례하므로 큰 아카이브에서 작은 파일 하나를
꺼낼 때 두드러진다.

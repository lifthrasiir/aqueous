# 10gb 코퍼스: 남은 일

Matt Mahoney의 10 GB 벤치마크(`/tmp/testdata/10gb`, 파일 79,431개, 10.00 GB)를 `aqr`로 압축한
결과를 종류별로 나누고, `other-projects.md`(11절)의 크기 예산과 견주어 남은 일을 적는다.

## 측정

- 2026-10-26, Apple M4(10코어), `aqr /tmp/testdata/10gb`(모든 스레드, level mid, 배치 512 MB),
  커밋 `031a0f6`. 3.570 GB, 압축 65.2 s, 최대 RSS 6.6 GB; 풀기 23.8 s.
- 같은 내용 파일 13,820개는 한 번만 셌다(그 몫은 0). 종류는 경로, 확장자와 탐지한 포맷으로
  나눴고, 크기는 `aqr` 로그의 파일별 몫(파일이 slice하는 블록을 slice한 바이트에 비례해 나눈 것)을
  더한 것이다.
- FASTA는 그 뒤 aqr-fasta(커밋 `0d908d1`)로 다시 쟀다. hg19 디렉토리(98개 파일, 3.24 GB)가
  971 → 706.7 MB이다. 10gb 전체로는 3.266 GB, 압축 92.4 s, 최대 footprint 6.7 GB이다
  (2026-10-26, 커밋 `ac0c17f`).
- 파일 사이의 내용 공유(2번) 뒤: 2.81 GB(−458 MB), 압축 112.5 s, 최대 footprint 8.9 GB.
- aqr-bundle(ar, tar, cpio) 뒤: 2810.8 → 2775.0 MB(−35.8 MB), 압축 109.8 → 106.0 s, 최대 footprint
  8.59 → 7.49 GB(2026-10-26, 커밋 `d150880`, 같은 날 `e6ec924`와 견줌).

## 종류별

| 종류 | 파일 | 원본 MB | 압축 MB | 비율 | 예산 MB |
|---|---|---|---|---|---|
| FASTA (hg19) | 93 | 3200 | 971 → 약 707 (aqr-fasta) | 0.30 → 0.22 | 650–720 |
| ZIP류 (JAR, ODF, EPUB 포함) | 761 | 652 | 608 | 0.93 | 잔여 ~100 |
| 그 밖의 아카이브 (PPMd, ZPAQ, 7z, LZMA) | 196 | 547 | 544 | 0.99 | 줄일 수 없는 것 580에 포함, PPMd 205 별도 |
| PE | 2795 | 1189 | 347 | 0.29 | 260–300 |
| enwik8, enwik9 | 2 | 1100 | 316 | 0.29 | 210–250 |
| 그 밖 | 7811 | 556 | 252 | 0.45 | ~250 |
| JPEG | 1480 | 236 | 190 | 0.81 | 196 |
| 텍스트 | 43202 | 786 | 131 | 0.17 | 80 |
| 이미지 (PNG, GIF, BMP, TIFF, PNM) | 2050 | 116 | 60 | 0.52 | |
| PDF | 151 | 65 | 52 | 0.80 | |
| ar | 4514 | 493 | 44 | 0.09 | 30 |
| WAV | 52 | 54 | 31 | 0.57 | |
| tar | 4 | 82 | 13 | 0.16 | |
| gzip | 2500 | 8 | 8 | 0.96 | |
| 합계 | 65611 | 9084 | 3568 | 0.39 | 약 2450–2650 |

가장 큰 파일들(압축 MB / 원본 MB):

- `2011/www.mattmahoney.net/dc/enwik9.zip` 322.6 / 322.6, 그대로
- `benchmarks/enwik9` 283.9 / 1000.0
- `2011/www.mattmahoney.net/dc/enwik9.pmd` 184.0 / 184.0, 그대로
- `benchmarks/simple/w8002` 100.0 / 100.0, 난수
- `www.mattmahoney.net/dc/mingw_benchmark.zpaq` 88.3 / 88.3, 그대로
- `www.mattmahoney.net/dc/silesia.zip` 65.6 / 67.6
- `2011/www.mattmahoney.net/dc/enwik8.zip` 36.5 / 36.5, `text8.zip` 31.3 / 31.3, 그대로
- `benchmarks/enwik8` 32.5 / 100.0

## 남은 일 (이득이 큰 순)

1. **[완료] FASTA: −264 MB.** aqr-fasta(order-9 문맥 모델과 rANS). 풀기는 hg19에서 753 MB/s이다.
   - 압축은 108 MB/s에 그친다. 배치에 큰 염색체가 몇 개뿐이라 2코어 남짓만 쓴다. 배치의
     파이프라이닝이 필요하다(아카이버 쪽 일).
2. **[대부분 완료] 다른 파일에 그대로 있는 내용: −458 MB.**
   - 서로 담고 있을 수 있는 파일을 한 배치에 모은다(`aqr-compress`의 `together`). ZIP 중앙
     디렉토리와 gzip trailer의 CRC-32와 크기가 맞는 파일, 앞 64 KiB가 같은 파일(`enwik8` ⊂
     `enwik9`)이다. 계획은 10gb에서 0.9 s이다.
   - 배치의 파일이 담은 내용은 probe와 `smaller`가 공짜로 본다(`probe::Held`, 앵커 1/4096).
     그래서 `enwik9.zip`처럼 그냥은 그대로 두던 스트림도 profile 6으로 재현한다.
   - 스트림을 만든 뒤 slice끼리 나눠 갖는다(`Formula::share_slices`). 짧은 slice부터, 각
     slice를 앞서 남긴 slice들로 앞에서부터 자른다. 형식은 그대로이다.
   - 이득(파일별 몫의 차): `enwik9.zip`과 `enwik9` −316 MB, `enwik8.zip`과 `enwik8` −50 MB,
     `silesia.zip`과 `silesia/*` −88 MB. 같은 배치의 다른 공유(progressive/baseline JPEG 쌍 등)가
     약 −30 MB이다.
   - 비용: 압축 +20 s, 최대 footprint +2.2 GB(enwik 묶음 1.46 GB가 한 배치). 그 묶음의 풀기는
     2.0 → 5.0 s(CPU 0.7 → 25 s), footprint 1.7 → 4.2 GB이다.
   - 남은 것:
     - 재현의 레코드. Info-ZIP `-9`는 profile 6에서 내용의 2.6%(`enwik9.zip` 약 26 MB)이다.
       아래 표대로 profile 10이나 에뮬레이션이면 거의 0이지만 느리다. 높은 레벨에서만 쓸 만하다.
     - **[완료]** `gimp.tar`: aqr-bundle이 멤버를 꺼낸다. 1 KiB 이상인 멤버는 자기 slice를 갖고,
       `share_slices`의 하한은 1 KiB, `Held`에 드는 파일의 하한은 4 KiB로 내렸으며, `together`는
       tar·ar·cpio의 4 KiB 이상인 멤버로도 파일을 묶는다. `gimp.tar`와 그 파일들 −11.3 MB(tar는
       11.87 → 6.93 MB). 같이 따라온 것: 같은 사진의 baseline과 progressive JPEG −15.2 MB.
     - ZIP 엔트리 중 256 KiB 이상은 자기 slice를 갖는다. 그러지 않으면 `silesia.zip`의 tar
       엔트리(mozilla, samba, xml)가 따로 있는 같은 파일과 어긋나서 그 뒤가 모두 두 번 저장된다.
     - 묶음이 크면 배치가 그만큼 커진다(상한 2 GiB).
   - Info-ZIP `-9` 스트림의 재현. enwik8을 Info-ZIP `-9`로 만든 스트림(2026-10-26, 10스레드):

     | profile | 레코드 | 쪼개기 | 합치기 |
     |---|---|---|---|
     | 6 (aqr-zip의 기본) | 2.61 MB (2.6%) | 214 MB/s | 349 MB/s |
     | 10, 체인 규칙 | 413 KB (0.41%) | 86 MB/s | 196 MB/s (한 스레드 35 MB/s) |
     | 10, 같은 스트림을 zlib `-9`로 만든 것(에뮬레이션) | 3 KB | 29 MB/s (직렬) | 204 MB/s (한 스레드 37 MB/s) |

     - 에뮬레이션은 C 인코더와 같은 속도이다(zlib `-9`와 Info-ZIP `-9` 모두 enwik8에서 34–35 MB/s).
     - 에뮬레이션으로 쪼개기는 checkpoint를 앞 세그먼트의 에뮬레이션에서 얻어서 스레드를 써도
       직렬이다. 매치 끝에서 자르면 checkpoint가 토큰만으로 정해지므로 병렬로 바꿀 수 있을 것이다.
3. **PE −50~−90 MB, 텍스트 −50 MB, enwik9 −35~−75 MB.**
   - 앞의 둘은 주로 zstd 레벨 문제이다. 예산은 높은 레벨과 `--long`으로 잡았고, 지금 level mid는
     3–9에서 고른다. 버킷별 레벨을 올린다.
   - enwik9는 텍스트 전처리 모듈(xwrt식 단어 치환)이 필요하다.
4. **ar −14 MB.** COFF 오브젝트에 x86 필터와 테이블 모델. aqr-bundle이 멤버를 꺼내지만
   (+1.0 MB, 평균 1 KB인 오브젝트는 헤더와 함께 압축하나 따로 하나 비슷하다), 오브젝트를 다루는
   재압축기가 아직 없다.
5. **PPMd: 재현하지 않는다.** `enwik9.pmd`(184.0 MB)와 `enwik8.pmd`(21.4 MB)는 그대로 둔다.
   - 재현 자체는 된다. 인코더가 결정적이다: 두 파일의 본문(22바이트 헤더 뒤)은 21,388,274바이트
     중 앞 21,388,266바이트가 같고, 끝의 범위 코더 마무리만 다르다. 다만 var.J는 32비트
     포인터로 메모리를 세므로(단위 12바이트), 256 MB가 차서 cut-off가 일어나는 시점까지 같게
     하려면 7-Zip의 Ppmd8처럼 32비트 오프셋으로 옮겨야 한다.
   - 너무 느리다. 같은 설정(order 10, 256 MB, cut-off)의 7-Zip Ppmd8(var.I rev.1, enwik8 21.40 MB로
     var.J와 0.07% 차이)이 M4 한 코어에서 3.6–3.9 MB/s이다(메모리가 차지 않는 2 GB면 6.6 MB/s).
     두 파일을 다시 인코딩하는 데 약 300 s가 걸리고, restore method 1에는 모델을 새로 시작하는
     지점이 없어 나눌 수 없다. 풀기(지금 23.8 s)는 물론 압축 예산(40 MB/s로 약 250 s)도 이것
     하나로 넘는다.
   - 그 밖에 FORMAT.md에 sub-allocator와 cut-off까지 인코더 전체(C로 1,500줄 안팎)를 적어야 하고,
     단독 PPMd var.J 파일은 이 코퍼스 밖에서 거의 볼 수 없다(7z는 var.H, ZIP의 method 98은 var.I).
   - `enwik8.pmd`는 위대로 `enwik9.pmd`의 접두어나 다름없어서 파일 사이의 공유만으로 −21 MB가
     된다. 지금의 `Held`/`share_slices`가 이를 잡는지는 아직 보지 않았다.

1과 2로 2.81 GB이다. 3까지 하면 2.6–2.7 GB이다. 주 목표 2.4 GB에는 4와, 3을 넘는 몫이 더
필요하다. PPMd 없이는 확장 목표(2.27 GB)의 길이 텍스트와 PE 쪽밖에 없다.

## 함께 볼 것

- 메모리: 압축 최대 footprint 8.9 GB(배치 크기의 약 7배, 그리고 2번의 큰 묶음), 풀기 footprint
  4.8 GB(2번 전). 스트리밍을 제대로 구현하며 다시 본다.
- 16 MiB solid 블록 상한은 텍스트에서 약 4%의 손해이다(`other-projects.md`).

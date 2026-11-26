# Aqueous 오류 감지와 복구(recovery) 설계

아카이브를 백업에 쓰는 흔한 용례를 위해 Aqueous 포맷에 넣은 오류 감지와 복구의 근거를 남긴다.
정의와 절차는 명세에 있고, 이 문서에는 명세에 없는 근거, 기각한 대안, 측정, 남은 일만 둔다.
상태 표기는 `xform-design.md`와 같다: **[확정]** 합의됨, **[잠정]** 방향만 정함, **[제안]** 검토 전.
끝낸 일의 경과는 git 이력에 있다.

상태: segment 주기(§5.1), mirror block(§5.2), 암호화 아카이브의 mirrored header(§5.5), repair와
salvage(§5.6)는 명세와 코드에 있다. slice 해시(§5.3)와 RS parity(§5.4)는 명세(§Recovery sets)에만 있고
**구현이 없다**(코드에 RS 부호가 없음을 2026-11-27에 확인).

관련 명세와 코드:

- `spec/index.bs`의 §Integrity and recovery(§Checksums, §Archival profile, §Mirror block, §Mirrored archives,
  §Recovery sets, §Repair, §Salvage), §Encrypted archives(mirrored header), §segment-status, §Reserved block
  types(non-critical 타입), §Partial access.
- `crates/aqr-archive/src/mirror.rs`: mirror block의 생성(`Mirroring`), 검사, repair와 salvage.
  `envelope.rs`: mirrored header와 손상된 암호화 아카이브의 복호.
- `crates/aqr/tests/archival.rs`: `-A`로 만든 아카이브의 손상, truncation, 암호화 아카이브의 복원 시험.

---

## 0. 배경과 전제

- 주류 포맷(7z, zip, xz, zstd)에는 복구 기능이 없다. 외부 par2는 `.aqr`에도 동작하므로, 내장의 근거는
  파일 하나로 끝나는 편의, append와의 통합, 포맷 구조를 아는 salvage(손상의 피해를 해당 parcel로 묶음)이다.
- 범위는 **아직 읽히는 장치에서 일부가 썩은 경우**이다. 장치나 파일 전체의 손실(E5)은 별도 사본으로만
  막을 수 있다. 백업 실패의 주원인은 오히려 E5이므로, 이 한계를 명세의 비규범 노트에 적어 기능이 실제보다
  강하게 받아들여지지 않게 해야 한다(S4).

## 1. 선행 사례와 교훈

| 사례 | 방식 | 배울 점 |
|---|---|---|
| Par2 | 외부 파일, GF(2¹⁶) RS, slice마다 MD5+CRC32, 패킷마다 magic·자기 MD5, main packet 중복 | slice 해시로 모든 오류를 erasure로 바꾼다. 패킷은 스캔으로 찾는다. 인코딩 O(N·M)로 느리고 행렬이 MDS가 아니다 |
| Par3 (2022 alpha draft) | BLAKE3, 임의 선형 부호(Cauchy, sparse, explicit). "Par inside ZIP": 패킷 자리를 0으로 간주한 checksum | 패킷 자신을 보호하지 못한다. §5.4가 처음 택했다가 기각한 방식 |
| RAR recovery record | 아카이브 안에 n%를 RS로 | "파일 하나로 끝난다"는 편의가 실제로 먹힌다 |
| FreeArc `-rr` | 섹터 XOR(연속 손상만 복구). FastECC: GF(0xFFF00001)의 NTT RS, O(n log n) | 대용량에는 O(n log n) 부호가 필수. GF(p)는 이진 데이터에 안 맞아 GF(2¹⁶)을 택한다 |
| dvdisaster RS03 | 255 layer로 나눠 부호어가 layer마다 1바이트 | 국소 손상과 복구 데이터가 같이 날아가지 않게 하는 배치 |

Par2의 MDS 결함: 지수 조합에 따라 특이 행렬이 생긴다(par2cmdline issue #389). 교훈: 행렬을 고정할 때는
*모든* 부분 행렬이 가역임이 증명된 구성(Cauchy, 또는 평가점이 서로 다른 RS)을 쓴다.

공통 교훈: **진짜 error correction은 필요 없다.** 저장 바이트 기준 slice 해시만 있으면 모든 손상이
위치를 아는 erasure가 되고, erasure 복구는 단순하고 강력하다.

## 2. 오류 모델 [잠정]

### 2.1 현실의 손상 (설계에 쓴 결론만)

- **HDD의 latent sector error는 뭉친다**(Bairavasundaram 2007, Schroeder 2010: LBA와 시간 모두).
  오류가 있는 디스크에서 반경 10 MB 안에 이웃이 있을 확률이 0.5 안팎이고, 버스트 길이는 Pareto 꼬리를
  가진다. 디스크 내부 redundancy의 평가(Schroeder 2010)에서 single/interleaved parity는 예상보다 훨씬
  약하고 MDS가 가장 강하다. 이 수치는 2004–2007년경 NetApp HDD의 것이고 거리는 논리 섹터 기준이다.
  → 교훈: interleave만으로는 부족하고 사본(§5.2)이나 MDS 부호(§5.4)가 맞다. 오류가 LBA 앞쪽에 몰리므로
  사본은 *다른 위치*에 둔다.
- **조용한 손상**(lost/misdirected/torn write, Bairavasundaram·Krioukov 2008): 단위는 블록 통째이고
  손상 블록의 40–50%는 이웃도 손상이다(평균 연속 3.4블록, 3%는 100블록 이상). 같은 블록 번호가 여러
  디스크에서 반복되므로 중복 구조는 서로 멀리 둔다. Krioukov: 손상 블록으로 parity를 갱신하는 parity
  pollution이 대표적 결함.
  → 교훈: repair는 손상된 원본으로 복구 데이터를 갱신하면 안 된다. repair가 원본을 덮어쓰기 전에 사본의
  checksum을 검증하고(§5.2), append 때 parity를 제자리에서 갱신하지 않는다(§5.4).
- **SSD**(Schroeder 2016, Google): 4년 안에 정정 불가 오류를 겪는 드라이브가 20–63%, bad block 2–4개
  뒤에 수백 개가 뒤따를 확률 50%. 순차로 쓴 아카이브는 같은 erase block에 들어갔을 가능성이 높아
  **연속 수십 MiB가 통째로** 사라질 수 있다(추론; 현행 3D NAND의 erase block은 수십 MB, ZNS/FDP의
  superblock은 수백 MB에서 GB). 그래서 erase block 손실은 E2가 아니라 E3이다. §5.2의 D ≥ 4 MiB는 HDD의
  LSE 클러스터를 겨냥한 값이고 SSD의 erase block 손실은 막지 못한다(M1에서 D를 정한다).
- **파일시스템과 전송**: 크래시 뒤의 0 구간, extent 손실의 큰 구멍, 그리고 압도적으로 흔한
  **truncation**(복사 중단, 용량 초과, 업로드 실패).
- **생성 시점의 RAM 비트 플립**은 어떤 ECC로도 못 막는다. 쓴 직후의 검증으로 대응한다.

### 2.2 오류 클래스

| 클래스 | 형태 | 빈도 | 목표 |
|---|---|---|---|
| **E1** 단일 단위 | 4 KiB 정렬 단위 하나의 erasure 또는 내용 오류 | 가장 흔함 | **항상** 견딘다 |
| **E2** 짧은 버스트 | 연속 수십 KiB ~ 수 MiB | 흔함 | 아카이브가 충분히 크면 **보장** |
| **E3** 큰 영역 손실 | 수십 MiB 이상 (extent, erase block, die) | 드묾 | best-effort. 전체 RS를 쓰면 정량적 보장 |
| **E4** truncation | 임의 길이의 꼬리 손실 | **매우 흔함** | 손실 상한을 설계로 묶는다 |
| **E5** 장치나 파일 전체 손실 | | 흔함 | 범위 밖 |
| **E6** 삽입, 삭제, shift | | 드묾 | 범위 밖 |

- 손상 단위는 **파일 오프셋 기준 4 KiB 정렬**이다(archive_start 기준이 아니다. 아카이브가 파일 처음에서
  시작하지 않을 수 있다).
- 오류끼리는 공간적으로 상관되어 있다. 핵심 방어는 하나의 손상이 여러 사본을 동시에 덮치지 않게 멀리
  떨어뜨리는 것이다.
- E6은 주로 버그 있는 도구나 텍스트 모드 전송에서 생긴다(Par2의 rolling CRC 재탐색이 대응). v1 범위 밖.

## 3. 명세의 유불리 (설계의 출발점)

유리: 모든 블록 길이가 짝수(GF(2¹⁶) 심볼과 맞음); spacer처럼 무시해도 되는 non-critical 블록(leading digit
A–F)이 있어 복구 블록을 모르는 디코더도 읽는다; segment는 self-contained이고 블록 간 압축 문맥 공유가
없어(dictionary 제외) 손상 범위 밖의 파일을 살릴 수 있다(§Partial access); segment를 찾는 경로가 앞방향
스캔과 footer의 역방향 체인 둘이다; footer는 나머지에서 재구성된다.

불리: 블록 헤더에 sync marker가 없어 size 필드 하나가 깨지면 앞방향 스캔이 탈선한다; 기존 checksum은
디코딩된 데이터에 대한 것이라 저장 바이트의 손상 위치는 압축을 풀어 봐야 안다; `footer_size`가 u16이라
해시 목록을 둘 곳이 못 된다; metadata는 segment 끝에, footer는 아카이브 끝에 있어 가장 흔한 E4가 가장
중요한 구조를 먼저 지운다(§5.1, §5.2로 줄임); 암호화 아카이브는 앞부분이 깨지면 전부 잃었다(§5.5로 해결).

## 4. 설계 원칙 [확정]

1. **복구는 해석(semantics) 밖의 전처리이다.** repair는 손상된 바이트열을 받아 recovery 데이터로 검증되는
   바이트열을 내놓는다. 암호화 아카이브의 복호도 같은 층이다. 명세의 `interpret an archive`와 "invalid
   archive는 입력만의 성질이다"는 바뀌지 않는다(§Repair).
2. **recovery 관련 블록은 무시해도 되는 블록이다.** spacer처럼 블록 사이 어디에나 올 수 있고, segment에
   속하지 않으며, 마지막 metadata block 뒤에 와도 된다.
3. **오류는 erasure로만 다룬다.** 위치는 읽기 실패, age 청크의 인증 실패, 블록 checksum, 저장 바이트의
   해시(§5.3, §5.4)로 안다.
4. **복구 데이터는 스스로 찾아질 수 있어야 한다.** 블록마다 sync magic과 자기 해시를 둔다.
5. **부호와 배치 규칙은 명세에 완전히 고정한다.** 행렬과 평가점까지 고정해 모든 구현이 같은 결과를 낸다.

## 5. 단계별 설계

### 5.1 segment 주기 (E4) [확정, 구현됨]

truncation이 일어나면 마지막 segment는 metadata를 반드시 잃는다(스트리밍 인코더는 metadata 사본을 앞에 둘
수 없다). `aqr -S SIZE`(기본 256 MiB, 출력 크기 기준, 그 크기를 넘긴 batch 뒤에서 닫음)로 주기적으로
segment를 닫아 손실을 **닫히지 않은 마지막 segment 하나로 묶는다**. 잘린 segment의 leaf block은 이름 없는
파일로 건지고, metadata 바로 뒤에서 잘리면 `segment-status`로 알린다(§Salvage, §segment-status).
각 segment는 module, resource, dictionary를 다시 쓰고(internal import 없음) 디렉토리·내용 없는 entry는 첫
segment에, 하드링크 그룹은 음수(archive-wide)로 쓴다.

기본값의 근거(10gb, M4, level mid, 79,431 파일). segment는 batch 경계에서만 닫히므로 기본 `-B 512M`에서는
batch 하나의 출력(평균 약 120 MB)보다 잘게 나뉘지 않는다.

| 설정 | segment | 크기 (B) | 증가 | 압축 | heap peak | 풀기 `-K` |
|---|---|---|---|---|---|---|
| `-S 0` | 1 | 2,464,041,292 | | 166.8 s | 2.83 GB | 31.0 s |
| `-S 256M`(기본) | 7 | 2,465,370,809 | +0.054% | 171.7 s | 2.88 GB | 32.5 s |
| `-S 64M` | 17 | 2,468,243,897 | +0.171% | 185.3 s | 2.87 GB | 34.2 s |
| `-B 128M -S 64M` | 24 | 2,471,295,915 | +0.294% | 216.8 s | 2.82 GB | 34.3 s |
| `-A`(기본 `-S`) | 7 | 2,476,454,152 | +0.505% | 198.3 s | 2.97 GB | 37.7 s |

- segment당 비용(222–315 KB)은 대부분 다시 쓰는 module block이다. 명령어 수는 segment 수와 무관하다(압축
  ±0.5%). batch를 줄이면(`-B 128M`) worker가 52%만 바빠 30% 느려지므로 segment를 잘게 하려고 batch를
  줄이지 않는다. 풀기 벽시계 31 → 34 s는 명령어가 그대로라 잡음일 수 있다.
- `-A`의 mirror는 기본 `-S`에서 11.08 MB(+0.45%).
- `-A` 아카이브를 60%에서 자르면 40,103개 파일(7.45 GB)이 이름째 나오고, 잘린 segment에서 90.2 MB를
  버리되 온전한 leaf에서 이름 없는 파일 5개(311.88 MB)를 건진다.
- 결정: 더 잘게 해서 얻는 것은 이름 정도이고 64M은 비용이 세 배이므로 256M을 유지한다. 크기 대신 시간으로
  닫는 선택지는 `append-design.md`("commit 지점").

### 5.2 mirror block: 중요 구조의 사본 (E1, E2) [확정, 구현됨]

같은 1바이트 손상도 위치에 따라 잃는 양이 다르다: leaf/non-leaf 블록은 그 내용, formula block은 파일 전체,
dictionary/module/resource block은 그것을 쓰는 모든 파일, metadata block은 segment 전체의 이름과 구조,
header/footer/size hint는 재구성 가능. 중요 구조는 보통 아카이브의 1% 미만이므로(M1) 사본이 싸고, 차등
보호로 **손상된 바이트가 속한 parcel만 잃는** 수준이 된다. metadata만 살아 있으면 `file_offset`으로 parcel에
직접 접근할 수 있어 sync marker 없는 스캔에 덜 의존한다.

> RS가 아니라 사본을 쓰는 이유: 대상이 작아 공간 비효율이 무시할 수준이고, slice 격자·set·interleave·shard
> 상한·append 때의 parity 갱신 문제가 모두 사라지며, segment를 쓸 때 그 segment의 사본만 쓰면 되어 append와
> 맞는다.

정의는 명세 §Mirror block. 명세에 없는 근거와 기각한 대안:

- 처음 제안의 `target_hash`는 뺐다(원본 checksum이 손상을 감지하고 `mirror_checksum`이 사본을 검증).
  원본(metadata, dictionary, module, resource, formula)의 checksum 필수화는 이 설계에서 나왔다(§Checksums 노트).
- 오프셋을 mirror 자신 기준으로 두면 archive_start나 체인이 깨져도 원본 위치를 안다.
- magic은 8바이트 하나(overlay만 따로). 후보 탐색에만 쓰고 확정은 mirror type, size 폭, `mirror_checksum`이
  하므로 길이는 오탐 비용만 정한다. 압축 데이터에서 우연한 일치의 기댓값은 1 TiB당 8바이트면 약 6×10⁻⁸개
  (6바이트 0.004개, 4바이트 256개). 값의 조건: 0x00/0xFF 연속 없음(0 채움, 지워진 flash), ASCII로 안 읽힘,
  자기 겹침과 다른 magic과의 앞뒤 겹침 없음. target type별 magic은 스캔 패턴만 늘려 두지 않는다.
- `archive_offset`(중첩 아카이브 구별)의 근거는 명세 노트. 아카이브별 무작위 ID는 같은 아카이브의 이전
  세대가 들어 있으면 사본을 못 가려 기각. archive_start 추정에 다수결을 안 쓰는 것은 안쪽 아카이브의
  mirror가 더 많을 수 있어서이다.
- 원본의 size 필드를 넓히라는 요구는 기존 아카이브에 나중에 mirror를 못 붙이게 해서 기각. mirror 쪽에
  widened 표시를 둔다(방향 비트가 필요해지면서 widened 비트를 `archive_offset`으로 옮겼다).

배치: 거리 D(SHOULD 4 KiB, 가능하면 4 MiB)는 명세. 길이 L인 버스트는 D < L일 때만 원본과 사본을 함께
지운다. D는 잠정이고 SSD erase block을 막으려면 더 커야 한다(M1). E3은 거리를 아카이브 비율로 잡으면
노릴 수 있으나 append와 충돌하기 쉬워 best-effort.

`aqr -A`(`Mirroring`): 마지막 metadata의 mirror만은 마지막 4 MiB 이상의 batch를 붙잡아 두고 그 앞에 쓴다.
나머지는 마지막 metadata 뒤에 원본 순서로 모으고, 다중 segment에서는 segment k의 mirror를 k+1의 metadata
바로 앞에 쓴다(거리는 segment 하나, 사본은 segment 두 개 분량까지만 든다).

> 꼬리에만 모으면 truncation이 마지막 metadata와 그 mirror를 함께 지운다. 마지막 batch들을 붙잡아 두면 그보다
> 짧은 truncation에서 metadata를 되살리고 footer를 다시 만든다(다중 segment 없이 E4를 막음, 메모리는 한 batch
> 남짓). 나머지 mirror는 다른 mirror를 사이에 두어 거리를 채우는 것이 가장 싸고, footer 앞 바이트를 안 바꾸는
> 출력 어댑터라 단일 파일 아카이브에도 쓰인다.
>
> 측정(cloud VM): 156 KB gzip과 그 원문 617 KB의 디렉토리에서 183,467 → 267,810 B(module block 두 개의 사본).
> 작은 아카이브는 module 크기만큼을 한 번 더 낸다. 10gb에서는 +0.45%(§5.1).

### 5.3 slice 해시 인덱스: 감지 전용 [잠정, 미구현]

§5.4의 index block만 두고 parity block이 없는 set(`parity_count` = 0)으로 표현한다. S = 64 KiB에서
오버헤드 0.0125%. §5.2가 있으면 각 파일의 checksum으로 대부분 대체되므로 우선순위는 낮다.

### 5.4 전체 RS parity (E3) [잠정, 미구현]

파일 내용까지 복구하려는 선택 기능. 정의는 명세 §Recovery sets(C1–C7; C0은 parity block이 255바이트를
넘어서 예약)와 §Repair. 아래는 명세에 없는 근거와 기각한 대안이다.

**보호 범위와 배치의 분리.** recovery block도 블록이라 블록 경계에만 놓이고 data block은 길 수 있어, 일정
간격 배치를 보장할 수 없다. 그래서 보호 범위는 블록 경계와 무관한 임의의 바이트 구간으로 하고 제약은
배치에만 둔다. 앞선 set의 recovery/mirror block은 뒤 set의 범위 안에서 다른 바이트처럼 보호된다.

> 기각: `[archive_start, archive_end)` 전체를 덮고 recovery/mirror 영역을 0으로 간주(Par3의 "Par inside
> ZIP"). 헤더가 깨진 recovery block은 영역을 몰라 멀쩡한 slice까지 해시 불일치가 되고, 영역 목록을 index에
> 두면 스트리밍에서 순환이 생기며, recovery 자신이 보호받지 못한다.
>
> 기각: 블록 크기 상한. 큰 파일이 formula block과 non-leaf 사슬로 쪼개져 formula가 모든 slice 길이를 먼저
> 선언해야 해서 스트리밍이 깨진다. 상한이 주는 것은 parity 대기 시간의 상한뿐이고 임시 파일로 해결된다.

마지막 set의 recovery block은 어느 set으로도 보호되지 않고 E4는 마지막 범위와 그 parity를 함께 지운다
(parity가 범위 뒤에 와야 해 구조적으로 불가피. mirror처럼 반대쪽에 두면 순환). segment 주기(§5.1)처럼
범위를 주기적으로 닫아 손실을 묶는다.

**블록과 slice.**

- 격자 원점은 `archive_start`가 아니라 header magic의 시작(근거는 명세 노트). SFX처럼 원점이 4 KiB 정렬이
  아니면 E1 하나가 slice 둘을 쓸 수 있으나 감수한다.
- set의 정체는 (`range_start`, `range_length`, S, p). 부호가 고정이라 같으면 내용도 같다(`set_id` 해시는 뺐다).
- parity block 하나에 shard 하나: 블록 손상이 정확히 shard 하나의 손실이고 단독 검증된다. index block은 두
  벌 이상 두라는 권고(Par2의 main packet 중복과 같은 발상).
- 암호화 아카이브에서도 recovery set은 평문 안에 둔다(격자와 복원 절차를 하나로). ciphertext 위(청크 사이의
  non-critical 프레임)에 두면 키 없이 repair할 수 있으나 블록 구조 밖의 두 번째 RS 구조가 된다. 평문의 비용은
  S ≥ 64 KiB(청크와 정렬)와 키 없는 repair의 부재.
- magic은 base64로 읽힌다: `RECOVERY_MAGIC` = `aQrRec0VerY=`, `OVERLAY_RECOVERY_MAGIC` = `Rec0Very4Q0=`
  (`0A`/`0D`가 들어 텍스트 모드 전송의 손상이 드러남. 처음의 overlay 쪽 `Rec0VerYaQo=`는 앞과 겹쳐 바꿨다).

**부호.** additive-FFT RS(Lin–Al-Naffouri–Han–Chung 2016; Leopard-RS, `reed-solomon-simd`)의 high-rate 부호와
같게 정해 그 구현을 쓸 수 있게 했다. 평가점이 서로 다르면 MDS가 증명된다(§1). 정의는 `reed-solomon-simd`
3.1.0을 블랙박스로 돌려 얻었다(R6; 라이브러리 소스는 읽지 않았다).

- **Cantor 좌표**로 심볼을 읽어야 인코더가 GF(2¹⁶)-선형이다(다항식 기저로는 네 모양 모두 불일치). β는
  공개 상수 `engine::CANTOR_BASIS`와 같다.
- **배치**는 Leopard2 문서의 "legacy high V1"(`[parity 0..T-1][message T..T+K-1][shortened zero tail]`,
  T = ceil_pow2(p)).
- **coset 식**(명세의 군별 보간)이 라이브러리의 생성 행렬과 일치함을 (n, p) = (8,4), (5,3), (40,20), (70,20),
  (1000,100), (3000,1), (61439,4096)에서 확인했다.
- **(n, p) 제약**: high rate는 n + p ≤ 65536(`HighRate::supports`: (61440, 4096)은 되고 (61441, 4096)과
  (61440, 4097)은 안 됨). p > n도 정의된다.
- **함정**: `reed-solomon-simd`의 low rate는 data를 ω<sub>0..</sub>에, parity를 ω<sub>m′+j</sub>에 두는 다른
  부호이고, 고수준 API(`encode`, `ReedSolomonEncoder`)는 p > n이면 low rate를 고른다. 구현은
  `rate::HighRateEncoder`를 명시한다. n = p가 2의 거듭제곱이면 같다.
- **심볼의 바이트 배치**: 바이트 하나만 켠 입력으로 확인했고 연속 u16le로 두면 입력 전체를 전치해야 하는데,
  이 배치는 두 구현 모두 복사 없이 쓴다(Leopard도 같다).
- **Leopard 호환**: 옛 API(`leo_encode`, 커밋 `910ad0a`)는 parent N = ceil_pow2(n + p)가 256을 넘으면 바이트
  단위로 같고, N ≤ 256이면 GF(2⁸)을 골라 달랐다. 명세는 작은 set에도 GF(2¹⁶)을 쓴다(체를 범위로 바꾸면
  정의가 둘이 되므로). Leopard로 만들려면 Leopard2 API에서 GF16을 명시한다.
- 속도(R5): Leopard AVX2 단일 코어 1.2 GB/s 수준(2017 README); `reed-solomon-simd`(Ryzen 5 3600 단일 코어)
  1 KiB 조각 32:32 10.2 GiB/s, 1024:1024 4.8 GiB/s, 32768:32768 1.6 GiB/s 인코딩, 1024:1024 디코딩
  1.3–1.4 GiB/s.
- **스트리밍 인코딩**(메모리 2·m·S, m < 2p): 고수준 API는 원본을 모두 들고 있어 안 된다(n = 16384, S = 16 KiB
  에서 p = 64와 1024 모두 최대 RSS 267 MiB). 공개 저수준 API로는 된다: 군 t(data shard tm..tm+m−1)를 모은
  버퍼에 `Engine::ifft(buf, 0, m, m, (t+1)·m)` 후 작업 버퍼에 XOR, 마지막에 `Engine::fft(acc, 0, m, m, 0)`이
  `HighRateEncoder`의 출력과 바이트 단위로 같다. `pos`, `truncated_size`, `skew_delta`는 문서가 없어
  시행착오로 찾았으므로 버전을 고정해 고수준 API와 비교하는 시험을 두거나 논문에서 직접 구현한다.
- 기각: fountain code(MDS 아님, RaptorQ 특허). Cauchy 행렬 직접 곱(메모리는 O(p·S)이나 입력 심볼마다 p번의
  곱셈이라 p가 수백이면 너무 느림).

**크기와 interleave.** 견디는 버스트와 1 GiB 예시는 명세 노트(m = 256에서 인코딩 메모리 약 32 MiB). E3을
노리면 R = 16 GiB, S = 512 KiB(n = 32768), r = 1%에서 약 160 MiB. s를 고르는 절차는 decoder가
`lg_slice_size`를 읽기만 하므로 권고로 둔다. **interleave는 두지 않는다**: 같은 총 parity로 견디는 버스트 길이는
G·p·S로 같고, 얻는 것은 shard 수 상한 아래에서 S를 줄이는 것뿐인데 전체가 MDS가 아니게 된다(Schroeder 2010의
interleaved parity가 약한 것과 같은 문제). S를 줄이고 싶으면 R을 줄인다.

**append.** 기존 parity를 제자리에서 갱신하지 않는다(새 set이 새 바이트와 옛 꼬리를 덮음; 명세 노트). 처음의
RS 선형성을 이용한 제자리 갱신 옵션은, 범위가 분리되면 필요 없고 crash 중의 parity 손실과 parity pollution
위험만 남아 뺐다.

### 5.5 암호화 [확정, 구현됨]

명세 §Encrypted archives. 헤더 필드의 사본(mirrored header)을 ciphertext 청크 사이에 두고 mirror block은
평문 안에 둔다. 인증 실패 청크도 건너뛰며 복호하면 평문 오프셋이 그대로라 mirror의 `target_offset`이
작동한다. 명세에 없는 근거:

- 사본에는 payload nonce가 들어간다(age 헤더 81바이트만 복제하면, nonce가 헤더 바로 뒤라 함께 지워져 쓸모
  없다). 그래서 nonce를 envelope 필드로 옮겼다(바이트 배치는 그대로).
- 사본의 위치는 오프셋이 아니라 인덱스로 기록한다(고정 폭 필드라 사본 하나로 archive_start와 모든 청크 위치가
  닫힌 식으로 나온다).
- 간격은 1, 2, 4, …, 64청크로 늘리다 64청크(평문 4 MiB, §5.2의 D)에서 멈춘다. 무한히 두 배로 늘리면 앞부분
  손상에는 강하나 마지막 사본이 파일 중간에 있어 앞부분 손상과 truncation이 겹치면 살아남는 사본이 없을 수
  있다(사본은 117바이트라 개수를 아낄 이유 없음). ext2/3/4의 sparse_super 배치는 사본이 비쌀 때를 위한 것이라
  맞지 않는다. 상한을 D에 묶었으므로 D가 바뀌면 함께 바뀐다(처음 16청크는 근거 없는 별도 숫자였다).
- 사본을 아카이브마다 구별하는 필드는 없다(무작위 salt와 nonce가 ID 역할).
- magic은 둘: `ENCRYPTED_MIRRORED_HEADER_MAGIC`(사본이 있는 형식. 사본이 선택이므로 기존
  `ENCRYPTED_HEADER_MAGIC` 유지; 마지막 2바이트를 버전 필드로 서술하지 않아 모르는 값은 invalid archive),
  `ENCRYPTED_MIRROR_MAGIC`(스캔용; `MIRROR_MAGIC`을 공유하면 비암호화 아카이브에 저장된 암호화 아카이브에서
  두 스캐너가 서로의 후보를 집는다).
- 원본 헤더의 `header_checksum`은 복구가 아니라 사본 없는 한 청크짜리 아카이브의 헤더 손상 감지용.
- 프레임은 짝수로 정렬하지 않는다(RS는 복호한 평문 위에서 동작).
- 남는 한계: 4 KiB 손상이 평문 64–128 KiB 손실이 된다(age 청크 크기가 고정이라 안쪽에서 해결 불가); 키 없이는
  손상 감지도 복구도 못 한다(mirrored header 복원은 예외). 키 없는 scrub이 필요해지면 ciphertext 해시
  인덱스나 parity를 non-critical 프레임으로 청크 사이에 넣는 확장을 검토한다.

### 5.6 repair 절차 [확정, 구현됨]

명세 §Repair와 §Salvage, 구현은 `aqr -d`/`-K`(`-t`는 인증에 실패한 청크가 있으면 거절). 처음 제안에서 바뀐
것:

- 처음에는 위치마다 인증됨/검증 안 됨/erasure(⊥)를 두고 미인증 바이트(인증 실패 청크를 키스트림으로 복호한
  것)를 켰을 때 파일 내용으로도 쓰려 했다. 명세는 "untrusted" 하나만 둔다. 미인증 바이트는 블록 경계를 찾는 데만,
  인증된 mirror가 알려 주는 블록 시작에 정확히 닿을 때만 믿는다. 파일 내용으로는 쓰지 않으므로 선택이 필요
  없다(ChaCha20은 malleable하므로 위조 입력이면 평문 비트를 마음대로 뒤집을 수 있다).
- ⊥는 두지 않는다. 잘려 없거나 믿을 수 없는 바이트는 블록째 spacer(non-leaf면 checksum이 안 맞는 non-leaf
  블록)로 바꿔 그 블록의 파일만 잃는다. placeholder 값으로 채우면 checksum 없는 leaf에서 그 값이 파일 내용이
  된다.
- recovery set(§5.4)의 복원은 mirror 복원과 번갈아 돌며 §Repair가 정의한다(구현 없음).

## 6. 범위 밖

E5(별도 사본), E6(필요해지면 rolling hash 기반 slice 재탐색을 따로 설계), 생성 시점 RAM 비트 플립(쓴 직후
검증), 악의적 변조(복구는 보안 기능이 아니다. 무결성과 인증은 암호화의 몫).

## 7. 열린 질문

1. **해시.** XXH64 하나로 통일할지, slice 인덱스에 XXH3-128을 쓸지(상대가 무작위 손상뿐이므로 64비트로
   충분하다고 본다).
2. **스크러빙.** 필요한 여유분은 "다음 검증 전까지 쌓일 손상"이 정하므로 `verify` 명령과 권장 주기를 도구
   쪽에서 함께 설계해야 한다.

## 8. 남은 일

문헌 조사(R1–R6)는 끝났고 결과는 위에 있다. 남은 것은 모두 미완이다(2026-11-27 코드에서 확인).

- [ ] **M1.** 기존 말뭉치(`scripts/zstd-dict-*`의 cargo, rustdoc, pystdlib 등)에서 metadata, dictionary,
  module, resource, formula block이 아카이브 크기에서 차지하는 비율을 잰다. mirror 오버헤드의 근거이고, 거리 D를
  erase block보다 크게 잡을지(§2.1, §5.2)도 이것과 함께 정한 뒤 명세 §Mirror block의 Issue를 정리한다.
- [ ] **M3.** (§5.4를 진행할 경우) FFT RS의 인코딩과 디코딩 처리량을 재서 압축 속도 대비 부담을 본다.
- [ ] **S3.** 블록 헤더가 손상된 뒤 다음 leaf block에서 metadata의 `file_offset`으로 재동기화하는 절차를
  §Repair에 넣는다(`other-projects.md`의 lzip 항목). non-leaf block은 순번으로 참조되므로 손상 지점 뒤로 같은
  parcel의 위치를 잃는다. (명세에 아직 없다.)
- [ ] **S4.** 오류 모델 요약과 E5가 범위 밖이라는 점을 비규범 노트로 명세에 추가한다.
- [ ] **I1.** E1–E4를 재현하는 fault injection 하네스(4 KiB 정렬 단위 손상, 버스트, 0 채움, truncation;
  위치는 무작위와 "중요 구조 조준"). 지금은 `crates/aqr/tests/archival.rs`와 `mirror.rs`의 시험이 정해 둔
  손상만 다룬다.
- [ ] **I3.** 복원한 아카이브를 파일로 쓰는 repair 명령. (스캔, mirror 복원, footer 재구성, salvage 추출은
  `aqr -d`/`-K`가 메모리에서 한다.)
- [ ] **I4.** `verify`에 slice 해시 검사(§5.3, §5.4). (checksum, archival profile, mirrored 여부와 mirror/원본
  일치는 `aqr -t`/`-K`가 보고한다.)
- [ ] **I5.** I1로 "손상된 바이트가 속한 parcel만 잃는다"는 목표를 E1과 E2(D 이상 아카이브)에 대해 검증.
- [ ] **I6.** (선택) §5.4의 recovery set 구현과, I1로 L ≤ r·R − S 한계의 검증. 스트리밍 경로를 고정 버전으로
  시험하는 것(R6의 남은 것) 포함.

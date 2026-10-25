# Aqueous 오류 감지와 복구(recovery) 설계

아카이브를 백업에 쓰는 흔한 용례를 위해, Aqueous 포맷에 오류 감지와 복구를 넣는 설계를 기술한다.
FreeArc가 이 기능을 진지하게 고려한 것이 계기가 되었다.
본문은 현재 제안된 설계를 서술하고, 그렇게 정한 근거는 인용 블록에 둔다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다. 선행 사례와 실증 연구 서술은 2026-10-21에 원문(논문, 명세, 구현 소스)으로 다시 확인했다(§8의 R 항목). 확인하지 못한 것은 본문에 그렇게 적었다.

관련 코드:

- `spec/index.bs`: 아카이브 포맷 명세 초안. 이 문서와 관련된 절은 다음과 같다.
  - §Block: 블록의 일반 구조와 짝수 길이 padding.
  - §Spacer block: 무시해도 되는 블록의 첫 선례.
  - §Reserved block types: leading digit(두 자리로 채운 첫 16진 자리)이 A–F인 non-critical 타입은 모르면 무시한다(ignored block).
  - §Content blocks: segment와 parcel, 그리고 "모든 content block은 뒤따르는 metadata block이 기술해야 한다"는 규칙.
  - §Unencrypted Archive: footer, `metadata_backward_offset`, u16 `footer_size`.
  - §Encrypted archives: age 기반 envelope와 mirrored header. 안쪽에서는 spacer와 trailing byte를 금지한다.
  - §Partial access: 부분 결과와 손상 감지 의무의 범위.
  - §Block types: 예약된 타입의 critical 여부는 leading digit으로 정해진다.
  - §Checksums, §Repair: 체크섬의 요약과 repair 관련 노트·이슈.

---

## 0. 배경과 전제

- 압축 아카이브는 백업 용도로 흔히 쓰인다. 그런데 7z, zip, xz, zstd 같은 주류 포맷에는 복구 기능이 없다. 사용자는 par2를 따로 돌리거나, 손상을 그냥 감수한다.
- 이 문서가 다루는 것은 **아직 읽히는 장치에서 일부가 썩은 경우**이다. 장치나 파일을 통째로 잃는 경우(§2의 E5)는 별도 사본으로만 막을 수 있으므로 범위 밖이다.
  > 백업 실패의 주원인은 오히려 E5이다. 이 한계를 명세의 비규범 노트에 적어 두어, 기능이 실제보다 강하게 받아들여지지 않게 해야 한다.
- 외부 par2도 `.aqr` 파일에 대해 이미 동작한다. 그래서 포맷에 내장할 가치는 세 가지로 정당화해야 한다.
  - 파일 하나로 끝나는 편의.
  - append(segment 추가)와의 통합.
  - 포맷 구조를 아는 salvage. 예를 들어 손상의 피해를 해당 parcel로 묶는 것이 여기에 해당한다.

## 1. 선행 사례

| 사례 | 방식 | 배울 점 |
|---|---|---|
| **Par2** | 외부 파일. GF(2¹⁶) RS, slice 최대 32768개. slice마다 MD5와 CRC32. 패킷마다 magic(`PAR2\0PKT`), 길이, 자기 MD5가 있고, main packet을 여러 번 중복 저장한다. | slice 해시로 손상 위치를 찾아 모든 오류를 **erasure로 바꾼다**. 패킷은 스캔만으로 찾을 수 있다. 단점: 인코딩이 O(N·M)이라 느리다. 또 **행렬이 MDS가 아니다**(아래). |
| **Par3** (2022-01-28 alpha draft, 미확정) | BLAKE3(하위 16바이트), 임의의 선형 부호. 행렬 패킷은 Cauchy(곧 RS), sparse random, explicit(LDPC용) 세 가지이다. Galois field는 8비트의 배수이면 된다. FFT 부호는 정의하지 않는다. | 부호를 고를 수 있다. **"Par inside ZIP"을 명시적으로 다룬다.** ZIP의 데이터와 central directory 사이에 Par3 패킷을 넣고, 패킷이 들어갈 자리를 0으로 간주한 checksum으로 파일 자신을 보호한다. 패킷 자신은 보호하지 못한다. §5.4가 처음 택했다가 기각한 방식이다. |
| **RAR recovery record와 .rev** | 아카이브 안에 크기의 n%를 RS로 덧붙인다. 볼륨 단위 복구용 .rev도 있다. | "파일 하나로 끝난다"는 편의가 사용자에게 실제로 먹힌다. |
| **FreeArc** | `-rr` recovery record. 0.36 문서 기준으로 섹터들의 XOR이고, 연속 손상만 record 크기까지 복구한다. 기본 크기는 아카이브 크기에 따라 4%/2%/1%이다. 0.67에서 바뀌었는지는 확인하지 못했다. 이후 Bulat Ziganshin의 FastECC가 나왔다. GF(0xFFF00001) 위의 NTT 기반 O(n log n) RS이고, 블록 2²⁰개까지, i7-4770에서 1.2 GB/s이다. 다만 v0.1은 인코딩만 구현했다. | 대용량에서는 O(n log n) 부호가 필수라는 문제의식. GF(p)는 이진 데이터에 맞지 않아 parity가 원본보다 조금 길다(4096바이트 섹터에 4100바이트). GF(2¹⁶)를 택할 이유이다. |
| **dvdisaster RS03** | 매체를 크기가 같은 255개 layer(데이터 n, CRC 1, ECC m)로 나누고, 각 RS 부호어가 layer마다 1바이트씩 가져간다. ECC는 별도 파일로 두거나 ISO 뒤에 붙인다(augmented image). | 국소 손상(긁힘, 연속 불량 섹터)과 복구 데이터가 같이 날아가지 않게 하는 배치. |
| 7z, zip, xz, zstd | 없음. | |

Par2의 MDS 결함: Par2는 Plank의 1997년 RS 튜토리얼을 따랐다. 그 튜토리얼의 행렬은 주장과 달리 항상 가역이 아니었다(Plank & Ding 2003의 정정 노트). Par2는 상수를 "위수가 65535인 2의 거듭제곱"으로 바꿔 연속 지수에 대해서는 문제를 막았다. 그러나 임의의 지수 조합에서는 여전히 특이 행렬이 생긴다. 예를 들어 지수 0과 21845에서 (2¹)²¹⁸⁴⁵ = (2⁴)²¹⁸⁴⁵이다(par2cmdline issue #389). Par3 draft는 이를 "사실상 무작위 행렬을 썼다"고 평하고 Cauchy 행렬로 고쳤다. 실제로 일어나는 일은 드물고, 복구 블록을 하나 더 구하면 대개 피할 수 있다.
> 이 설계의 교훈: 원칙 5에 따라 행렬을 고정할 때는 *모든* 부분 행렬이 가역임을 증명할 수 있는 구성(Cauchy, 또는 평가점이 서로 다른 RS)을 써야 한다.

공통 교훈: **진짜 error correction은 필요 없다.** 저장 바이트 기준 slice 해시만 있으면 모든 손상이 위치를 아는 erasure가 되고, erasure 복구는 단순하고 강력하다.

## 2. 오류 모델 [잠정]

### 2.1 현실의 손상

- **HDD.** 섹터마다 내부 ECC가 있어서, 무작위 비트 오류는 대부분 "섹터 읽기 실패"(latent sector error, LSE)로 나타난다. 조용히 비트가 뒤집히는 경우는 드물다.
  - Bairavasundaram 등(SIGMETRICS 2007)과 Schroeder 등(FAST 2010)에 따르면 LSE는 독립적으로 생기지 않고, **LBA상으로도 시간상으로도 뭉친다**. 짧은 버스트가 대부분이지만 긴 꼬리가 있다.
    - 규모(2007): NetApp 디스크 약 153만 개를 32개월 관찰했다. 기간 중 LSE를 겪은 디스크는 nearline 약 8.5%, enterprise 1.9%이다. 오류 디스크당 오류 수의 중앙값은 3이다.
    - 공간(2007): 오류가 2–10개인 디스크에서 기존 오류의 반경 10 MB 안에 다른 오류가 있을 확률은 대부분의 모델에서 0.5, 여러 모델에서 0.6 이상이다. 확률은 반경 100 KB와 1 MB 사이에서 크게 오른다.
    - 버스트(2010): 논리적으로 연속된 버스트의 90–98%는 섹터 하나이다. 꼬리는 기하 분포가 아니라 Pareto 분포에 맞는다. 오류의 20–60%는 10섹터 안에 이웃이 있다. 오류의 20–50%가 논리 공간의 처음 10%에 몰린다.
    - 시간: 오류의 40–80%는 직전 오류로부터 1분 안에 나타난다(2007). 55–85%의 디스크에서는 모든 오류가 같은 2주 안에 나타난다(2010).
  - Schroeder 2010은 디스크 내부 redundancy를 직접 평가했다. 이 설계와 가장 관련이 깊은 문헌이다.
    - single parity check(SPC)는 약하다. 실용적인 k(8 이상)에서 평균 약 4분의 1, 일부 모델에서는 50% 가까운 디스크에 오류가 둘 이상인 parity group이 생긴다. 1+1 구성에서도 평균 15%이다.
    - interleaved parity(IPC)는 원 제안자의 예측보다 훨씬 약하다. 긴 꼬리에 민감하기 때문이다. 8+1에서 16+2 MDS로 바꾸면 지표가 대부분 절반으로 준다. IPC로 비슷한 효과를 내려면 56+7이나 64+8이 필요하다.
    - MDS가 가장 강하다. 저자들이 제안한 column-diagonal parity(CDP)는 MDS에 가까운 신뢰성을 더 낮은 비용으로 낸다.
    - 스크러빙은 순차 방식에 비해 staggered 방식이 오류 감지까지의 평균 시간을 10–20%(최대 40%) 줄인다.
    > 수치는 모두 2004–2007년경 NetApp HDD의 것이고, 거리는 물리 위치가 아니라 논리 섹터 기준이다.
>
> 이 설계의 교훈: 버스트 길이의 꼬리가 길어서 interleave만으로는 부족하다. 사본(§5.2)이나 MDS 부호(§5.4)가 맞다. 또 오류가 LBA 앞쪽에 몰리므로 사본을 *다른 위치*에 두는 것이 중요하다.
- **조용한 손상.** Bairavasundaram 등(FAST 2008)과 Krioukov 등(FAST 2008)이 다룬 lost write, misdirected write, torn write, 펌웨어 버그가 원인이다. 단위는 비트가 아니라 **블록 하나가 통째로 틀린 내용**이고, 역시 공간적으로 뭉친다.
  - 규모(Bairavasundaram 2008): 153만 디스크, 41개월. 처음 17개월 동안 checksum 불일치가 생긴 디스크는 nearline 0.66%, enterprise 0.06%이다. 최악의 모델은 약 4%이다.
  - 공간: 손상 블록의 40–50%는 바로 옆 블록도 손상되어 있다. 연속 블록을 넘어서면 공간적 국소성이 거의 없다. 오류가 둘 이상인 디스크에서 연속 손상의 평균 길이는 3.4블록이다. 3%는 100블록, 0.7%는 1000블록 이상의 연속 손상을 겪는다. 같은 블록 번호가 여러 디스크에서 반복해서 손상되므로, 중복 구조는 서로 멀리 두라고 권한다.
  - identity discrepancy(lost/misdirected write)는 365개 디스크에서만 나타났다. checksum 불일치보다 한 자릿수 이상 적다.
  - Krioukov 2008은 RAID 보호 방식을 model checker로 검사했다. 검사한 모든 방식에서 결함을 찾았다. 손상된 블록이 parity 계산을 거쳐 다른 블록으로 번지는 parity pollution이 대표적이고, 스크러빙이 그 주원인이 되기도 한다. 해법으로 version mirroring, 블록 checksum, 물리·논리 식별 정보를 함께 쓰라고 제안한다.
  > 이 설계의 교훈: 복구 단계는 손상된 원본으로 복구 데이터를 갱신하면 안 된다. repair가 원본을 덮어쓰기 전에 사본 쪽 checksum을 검증하는 것(§5.2)과 append 때 parity를 제자리에서 갱신하지 않는 것(§5.4)이 여기에 해당한다.
- **SSD.** Schroeder 등(FAST 2016)은 Google의 SSD 10개 모델(MLC, eMLC, SLC, 24–50 nm)을 6년간 관찰했다.
  - 처음 4년 동안 정정 불가 오류(UE)를 한 번 이상 겪은 드라이브는 모델에 따라 20–63%이다. HDD 쪽 비교치는 32개월 동안 3.5%의 디스크에 bad sector가 생겼다는 것이다. 저자들은 SSD가 교체율은 HDD보다 낮지만 UE는 훨씬 많다고 결론지었다.
  - 30–80%의 드라이브에 bad block이 생긴다. bad block이 2–4개 생기면 수백 개가 뒤따를 확률이 50%이다. 2–7%의 드라이브에는 bad chip이 생긴다.
  - 섹터 단위의 공간 분포 분석은 없다.
  - 순차로 쓴 아카이브는 같은 erase block에 들어갔을 가능성이 높다. 그래서 erase block 하나를 잃으면 **논리적으로 연속된 수십 MiB가 통째로** 사라질 수 있다. 이 부분은 추론이다.
    - 현행 3D NAND의 erase block은 수십 MB이다. Micron의 2016년 3D TLC가 16 KiB 페이지 × 1536 = 24 MiB이고, ZN540 emulation 설정이 16 KiB × 768 = 12 MiB이다. 176층 이후 제품의 데이터시트 수치는 확인하지 못했다.
    > 그래서 erase block 손실은 E2가 아니라 E3에 넣었다. §5.2의 D ≥ 4 MiB는 HDD의 LSE 클러스터(반경 10 MB 안에서 이웃 확률 0.5)를 겨냥한 값이고, SSD의 erase block 손실은 막지 못한다. D를 erase block보다 크게(예: 64 MiB) 잡을지는 M1의 오버헤드 측정과 함께 정한다.
    - FTL은 여러 die와 plane에서 같은 번호의 블록을 묶어 superblock으로 쓴다. ZNS 논문(Bjørling 등, ATC 2021)은 16–128개 die에 걸친 stripe가 흔하고, 그러면 수백 MB에서 수 GB가 된다고 했다. Samsung FDP의 reclaim unit(= superblock)은 약 6 GB이다. die 하나나 superblock 하나를 잃으면 E3 규모가 된다.
  - FTL 장애는 장치 전체 손실(E5)로 이어진다.
- **파일시스템과 전송.** 크래시 뒤 0으로 채워진 구간, extent 손실로 생긴 큰 구멍, 그리고 압도적으로 흔한 **truncation**(복사 중단, 용량 초과, 업로드 실패)이 있다. 네트워크 전송 자체에서는 TCP와 TLS 덕분에 비트 오류가 사실상 없다.
- **생성 시점의 RAM 비트 플립.** 손상된 데이터를 그대로 인코딩하므로 어떤 ECC로도 막지 못한다. 쓴 직후의 검증으로 대응한다.

### 2.2 오류 클래스

| 클래스 | 형태 | 빈도 | 목표 |
|---|---|---|---|
| **E1** 단일 단위 | 4 KiB 정렬 단위 하나의 erasure 또는 내용 오류 | 가장 흔함 | **항상** 견딘다 |
| **E2** 짧은 버스트 | 연속 수십 KiB ~ 수 MiB (LSE 클러스터, 연속 블록 손상, 긁힘) | 흔함 | 아카이브가 충분히 크면 **보장**한다 |
| **E3** 큰 영역 손실 | 수십 MiB 이상 (extent 손실, flash erase block과 die, superblock 장애) | 드묾 | best-effort. 전체 RS를 쓰면 정량적으로 보장한다 |
| **E4** truncation | 임의 길이의 꼬리 손실 | **매우 흔함** | 손실 상한을 설계로 묶는다 |
| **E5** 장치나 파일 전체 손실 | | 흔함 | 범위 밖 |
| **E6** 삽입, 삭제, shift | | 드묾 | 범위 밖 |

전제는 두 가지이다.

- 손상 단위는 **파일 오프셋 기준 4 KiB 정렬**이다. archive_start 기준이 아니다. 아카이브가 파일 처음에서 시작하지 않을 수도 있기 때문이다(명세 §Unencrypted Archive).
- 오류끼리는 독립이 아니라 **공간적으로 상관**되어 있다. 따라서 핵심 방어는 하나의 손상이 여러 사본을 동시에 덮치지 않도록 멀리 떨어뜨리는 것이다.

> E6은 저장 장치에서 거의 일어나지 않고, 주로 버그 있는 도구나 텍스트 모드 전송에서 생긴다. Par2는 rolling CRC로 어긋난 slice를 다시 찾는데, 이것이 E6 대응에 해당한다. 이 기능은 v1 범위 밖이다.

## 3. 현재 명세의 유불리

유리한 점:

- **모든 블록 길이가 짝수이다**(block padding). GF(2¹⁶) 심볼(2바이트)과 그대로 맞는다.
- **Spacer처럼 무시해도 되는 블록이라는 개념이 이미 있다.** recovery 관련 블록을 같은 지위로 두면, 복구 기능을 모르는 디코더도 아카이브를 그대로 읽는다. 명세는 non-critical 타입(leading digit A–F)을 모르면 무시하므로, recovery 블록을 그 구간에 두면 된다.
- **segment는 self-contained이고 parcel 구조가 있으며, 블록 간에 공유되는 압축 문맥이 없다**(dictionary 제외). 복구에 실패해도 손상 범위 밖의 파일은 살릴 수 있고, 명세 §Partial access가 이미 이를 허용한다.
- **segment를 찾는 경로가 둘이다.** 앞방향 스캔과 footer에서 시작하는 metadata 역방향 체인이다.
- **footer는 나머지에서 재구성할 수 있다.** `size_hint`는 길이에서, `metadata_backward_offset`은 스캔에서 얻는다. 그래서 보호 범위에서 뺄 수 있다. header magic은 상수이고, size hint block은 footer와 중복이다.

불리한 점:

- **블록 헤더에 sync marker가 없다.** 대부분 1바이트 leb128 type과 size뿐이라, size 필드 하나가 깨지면 앞방향 스캔이 탈선한다.
- **기존 checksum은 디코딩된 데이터에 대한 것이다.**
  - 저장 바이트의 손상 위치를 찾으려면 압축을 풀어 봐야 한다.
- **`footer_size`가 u16이다.** 해시 목록 같은 인덱스를 둘 곳이 못 된다.
- **Encrypted archive는 앞부분이 깨지면 전부 잃는다.** scrypt 인자, file key, payload nonce가 맨 앞에만 있었다. mirrored header로 해결했다(§5.5).
- **metadata는 segment 끝에, footer는 아카이브 끝에 있다.** 가장 흔한 손상인 E4가 가장 중요한 구조를 먼저 지운다(§5.1).

## 4. 설계 원칙 [제안]

1. **복구는 해석(semantics) 밖의 전처리로 정의한다.** repair는 손상된 바이트열을 받아, recovery 데이터로 검증되는 바이트열을 내놓는 과정이다. 암호화 아카이브의 복호도 같은 층에 속한다(§5.6). 명세의 `interpret an archive`와 "invalid archive는 입력만의 성질이다"라는 원칙은 바뀌지 않는다.
2. **recovery 관련 블록은 무시해도 되는 블록이다.** spacer처럼 블록 사이 어디에나 올 수 있고, segment에 속하지 않으며, 마지막 metadata block 뒤에 와도 된다.
3. **오류는 erasure로만 다룬다.** 손상 위치는 저장장치의 읽기 실패나 age 청크의 인증 실패로 이미 알려져 있거나, 저장 바이트에 대한 해시로 찾는다. 입력 모델은 §5.6에 있다.
4. **복구 데이터는 스스로 찾아질 수 있어야 한다.** 블록마다 sync magic과 자기 해시를 두어, header나 footer, 체인이 깨져도 스캔으로 찾을 수 있게 한다.
5. **부호와 배치 규칙은 명세에 완전히 고정한다.** 행렬과 평가점까지 고정해, 모든 구현이 같은 결과를 내게 한다.

## 5. 단계별 설계

우선순위는 §5.1 > §5.2 > §5.4 순이고, §5.3은 선택이다.

### 5.1 segment 주기 (E4) [제안]

metadata는 데이터를 다 쓴 뒤에야 만들어진다. 그래서 스트리밍 인코더는 마지막 segment의 metadata 사본을 앞쪽에 둘 수 없고, truncation이 일어나면 마지막 segment는 metadata를 반드시 잃는다.

- compressor는 N MiB(또는 N초)마다 segment를 닫는다. 그러면 truncation 손실은 **아직 닫히지 않은 마지막 segment 하나로 묶인다**.
- 명세에는 비규범 권고로 둔다. 백업 도구가 기본값을 정한다.
- metadata를 잃은 마지막 segment의 leaf block도 이름 없는 파일로는 추출할 수 있다(photorec식 salvage).

> 대가도 있다. segment가 많아질수록 metadata 오버헤드가 늘고, 디렉토리가 segment마다 다시 나타난다(명세 §Merging segments). 기본 주기는 실측으로 정해야 한다(§8의 M2).

### 5.2 mirror block: 중요 구조의 사본 (E1, E2) [제안]

#### 동기

같은 1바이트 손상이라도 위치에 따라 잃는 양이 크게 다르다.

| 손상 위치 | 잃는 것 |
|---|---|
| leaf / non-leaf 데이터 블록 | 그 블록의 내용(파일 하나 또는 바이트 범위 하나) |
| formula block | 파일 전체. non-leaf 데이터가 멀쩡해도 소용없다 |
| dictionary / module / resource block | 그 블록을 쓰는 모든 파일 |
| metadata block | segment 전체의 이름과 구조. 역방향 체인도 끊긴다 |
| header, footer, size hint | 사실상 없음(상수이거나 재구성 가능) |

중요 구조는 보통 아카이브 크기의 1% 미만으로 예상된다(§8의 M1). 따라서 무작위 손상이 여기에 떨어질 확률은 낮다. 하지만 떨어지면 피해가 수백, 수천 배로 커진다. 차등 보호는 이 증폭을 없애서, **손상된 바이트가 속한 parcel만 잃는** 수준으로 피해를 줄인다.

부수 효과도 있다. metadata만 살아 있으면 `file_offset`과 formula의 오프셋으로 각 parcel에 직접 접근할 수 있으므로, sync marker가 없는 앞방향 스캔에 의존할 일이 줄어든다.

> RS가 아니라 사본을 쓰는 이유:
> - 보호 대상이 작아서, 사본의 공간 비효율이 전체 대비 무시할 수준이다.
> - slice 격자, set, interleave, slice 수 상한, append 때의 parity 갱신 같은 문제가 모두 사라진다.
> - segment를 쓸 때 그 segment의 중요 블록 사본만 쓰면 끝나므로, append와 잘 맞는다.

#### 구조 [잠정]

명세 §Mirror block에 반영했다. 아래는 명세에 없는 근거와 기각한 대안이다.

- 처음 제안의 `target_hash`는 뺐다. 원본 checksum이 손상을 감지하고, `mirror_checksum`이 사본을 검증한다.
- 오프셋을 mirror 자신 기준으로 두면, archive_start나 체인이 깨져도 원본 위치를 알 수 있다.
- 원본에 checksum을 필수로 둔 것은 이 설계에서 나왔다.
  > metadata의 checksum 없는 모드는 처음 초안부터 있었고 근거가 남아 있지 않다. 성능 때문이었다면, 이 블록들은 아카이브의 작은 일부이고 한 번씩만 읽히므로 XXH64 비용은 무시할 수준이다. 반면 이름과 구조는 다른 어떤 checksum으로도 검증되지 않는다.
- magic은 8바이트이고 target type과 관계없이 하나이다.
  > 길이: magic은 후보를 찾는 데만 쓰고, 확정은 앞의 mirror type, size 폭, `mirror_checksum`이 한다. 따라서 magic 길이는 정확성과 무관하고 잘못된 후보의 비용만 정한다. 압축된 데이터에서 우연한 일치의 기댓값은 1 TiB당 8바이트면 약 6×10⁻⁸개(6바이트면 약 0.004개, 4바이트면 약 256개)이다. Par2도 8바이트 magic(`PAR2\0PKT`)에 패킷 해시를 더한다.
  > target type별 magic은 두지 않는다. 바로 앞의 mirror type이 이미 그 정보를 담고 있고, magic이 여럿이면 스캔 패턴만 늘어난다.
  > 값의 조건: 0x00이나 0xFF가 연속되지 않을 것(0 채움, 지워진 flash), ASCII로 읽히지 않을 것, 자기 자신과 겹치지 않을 것, 다른 magic과 앞뒤가 겹치지 않을 것. 현재 값은 여덟 바이트가 모두 달라 자기 겹침이 없고, 나머지 조건도 만족한다.
- `archive_offset`은 중첩 아카이브 때문에 둔다.
  > 원본 자리의 checksum 실패 여부로는 안쪽 아카이브의 mirror를 막지 못한다. 그 자리는 원래 블록 경계가 아니므로 어떤 바이트가 와도 실패하기 때문이다. 같은 아카이브의 이전 세대가 들어 있어도 추정한 archive_start의 위치가 다르므로 구별된다. 아카이브마다 무작위 ID를 두는 방식은 그런 사본을 구별하지 못해서 택하지 않았다.
- mirror의 widened 표시.
  > 원본 쪽 size 필드를 넓히라고 요구하는 방식은 기존 아카이브에 나중에 mirror를 붙일 수 없게 만들어서 택하지 않았다.

#### 배치

명세 §Mirror block에 반영했다(E1, E2의 거리 SHOULD와 블록 종류별 배치 요령). 거리 D의 값은 명세에서도 잠정이다. 근거는 다음과 같다.

길이 L인 버스트는, 원본과 사본의 거리 D가 L보다 짧을 때만 둘을 함께 지운다.

- **E3:** 거리를 고정값이 아니라 아카이브 크기의 비율로 잡으면 노려볼 수 있다. 다만 append 구조와 충돌하기 쉬우므로 best-effort로 둔다.

#### 찾기

마지막 metadata가 깨지면 체인의 출발점이 사라진다. 그때는 `mirror_magic` 스캔에 의존한다. repair 모드에서 아카이브 전체를 한 번 읽는 비용은 감수한다. 후보의 확정 절차와 archive_start 추정은 명세 §Mirror block에 반영했다.

> archive_start 추정에 다수결을 쓰지 않는 이유: 안쪽 아카이브의 mirror가 바깥 것보다 많을 수 있다.

### 5.3 slice 해시 인덱스: 감지 전용 [제안, 선택]

- §5.4의 index block만 두고 parity block을 두지 않는 set(`parity_count` = 0)으로 표현한다. 별도 구조는 두지 않는다.
- 오버헤드는 S = 64 KiB에서 0.0125%이고, 압축을 풀지 않고도 손상 위치를 slice 단위로 짚을 수 있다.
- §5.2가 있으면 각 파일의 checksum으로 대부분 대체되므로 우선순위는 낮다.

### 5.4 전체 RS parity (E3) [제안, 선택]

파일 내용까지 복구하려는 사용자를 위한 옵션이다. §5.2가 이미 있으므로, 이 단계의 복잡도는 순수하게 내용 보호의 비용이 된다.

#### 보호 범위와 배치의 분리

recovery block도 블록이므로 블록 경계에만 놓일 수 있고, data block은 얼마든지 길 수 있다. 따라서 일정한 간격으로 recovery block을 넣을 수 있다는 보장은 없다. 이 설계는 보호 범위를 블록 경계에서 떼어 내어 이 제약을 배치 쪽에만 남긴다.

- **recovery set**은 보호 범위 `[range_start, range_end)`, slice 크기 S, parity 수 p로 정해진다. 보호 범위는 임의의 바이트 구간이다. data block 한가운데에서 시작하거나 끝나도 된다.
- 보호 범위는 `[archive_start, archive_end)` 안에 있어야 한다. header magic은 상수이고 footer는 재구성하므로 보호하지 않는다.
- **한 set의 모든 recovery block은 `range_end` 뒤에 온다.** mirror block의 "target block이 앞선다"와 같은 방향이다.
- 앞선 set의 recovery block과 mirror block은 뒤 set의 보호 범위 안에서 다른 바이트와 똑같이 보호된다.
  > 처음 제안은 `[archive_start, archive_end)` 전체를 덮고 recovery block과 mirror block 영역을 0으로 간주했다(Par3의 "Par inside ZIP"과 같은 발상). 기각한 이유는 다음과 같다.
  > - 헤더가 깨진 recovery block은 영역을 알 수 없다. 그 블록이 걸친 멀쩡한 slice까지 0으로 간주할 범위를 몰라 해시가 불일치하고 erasure가 된다. 영역 목록을 index에 두면 스트리밍에서 순환이 생긴다.
  > - recovery 데이터 자신이 보호받지 못한다. 범위를 분리하면 뒤 set이 앞 set의 recovery 데이터를 보호한다.
- 블록 크기 상한 같은 추가 조건은 포맷에 두지 않는다. 거대한 data block 때문에 parity를 내보내지 못하는 동안의 버퍼링은 compressor의 문제로 남는다(아래 배치).
  > 상한을 두면 큰 파일이 formula block과 non-leaf data block 사슬로 쪼개져야 한다. 그러면 formula block이 모든 slice의 길이를 먼저 선언해야 하므로 스트리밍이 깨진다(명세 §Unencrypted Archive의 노트). 상한이 주는 것은 parity 대기 시간의 상한뿐이고, 그것은 임시 파일로 해결된다.

#### slice 격자

- S = 2<sup>s</sup>, s ≥ 12이다.
- 격자 원점은 `archive_start`가 아니라 **header magic의 시작**(`archive_start` − 6)이다. slice i는 원점에서 [i·S, (i+1)·S)이다.
  > `archive_start`는 header magic 뒤이다. 여기에 맞추면 파일 처음에서 시작하는 보통의 아카이브에서 4 KiB 단위가 항상 slice 두 개에 걸친다. SFX처럼 원점 자체가 4 KiB 정렬이 아니면 E1 하나가 slice 두 개를 소모할 수 있는데, 이것은 감수한다.
- set의 data shard는 보호 범위와 겹치는 격자 slice들이고, 개수를 n이라 한다. 범위 밖 부분은 가상의 0으로 채운다. 따라서 부분 slice는 양 끝의 두 개뿐이다.
- 암호화 아카이브에서는 평문의 오프셋 0이 header magic의 시작이므로 같은 원점이 age 청크와 정렬된다. S를 64 KiB의 배수로 잡으면 slice와 청크가 정확히 겹쳐, 청크 하나의 인증 실패가 slice 하나의 erasure가 된다(§7의 8번).

#### 블록

non-critical 타입 둘을 쓴다. 예를 들어 B0–B3은 parity block, B4–B7은 index block이고, size 필드의 폭은 다른 블록처럼 타입 % 4로 정해진다. 데이터는 다음과 같다.

| 필드 | 타입 | 비고 |
|---|---|---|
| `recovery_magic` | u8[8] | 값의 조건은 §5.2의 `mirror_magic`과 같다 |
| `archive_offset` | leb128 | `block_start` − `archive_start`. mirror block과 같은 용도 |
| `range_offset` | leb128 | `block_start` − `range_start` |
| `range_length` | leb128 ≠ 0 | |
| `lg_slice_size` | u8 ≥ 12 | s |
| `parity_count` | leb128 | p. index block만 있는 set(§5.3)은 0 |
| (parity) `shard_index` | leb128 < p | |
| (parity) `shard` | u8[S] | |
| (index) `slice_hashes` | u64le[n] | data shard마다 보호 범위 안 바이트만의 XXH64 |
| `recovery_checksum` | u64le | 블록 시작부터 앞 필드까지의 저장 바이트에 대한 XXH64. `mirror_checksum`과 같다 |

- **set의 정체는 (`range_start`, `range_length`, S, p)이다.** 부호가 완전히 고정되어 있으므로(원칙 5) 이 넷이 같으면 내용도 같다.
  > 처음 제안의 `set_id`(파라미터의 해시)는 뺐다. 세대 구분은 범위가 하고, 아카이브 구분은 `archive_offset`이 한다. 무작위 ID를 쓰지 않는 것은 §5.2의 노선과 같다.
- parity block 하나에 parity shard 하나를 담는다. 블록의 손상이 정확히 shard 하나의 손실이 되고, 블록마다 스캔으로 찾아 단독으로 검증할 수 있다.
- index block은 최소 두 벌을 서로 D 이상 떨어뜨려 둔다(Par2의 main packet 중복과 같은 발상). 해시 목록이 없으면 조용한 손상의 위치를 알 수 없다.
- 압축을 풀지 않는 `verify`(I4)가 slice 해시만으로 아카이브 전체를 검사할 수 있다.

#### 부호

- 명세에서는 GF(2¹⁶) 위의 systematic RS로 정의한다. 차수가 n 미만인 다항식 f에 대해 data shard의 각 심볼은 f(x<sub>i</sub>), parity shard의 심볼은 f(y<sub>j</sub>)이다. 평가점이 서로 다르기만 하면 MDS가 증명된다(§1의 Par2 교훈).
- 평가점은 additive-FFT 기반 RS(Lin–Chung–Han 계열, Leopard-RS와 `reed-solomon-simd`)의 배치에 맞춰, 그 구현을 그대로 쓸 수 있게 고른다.
  - MDS이므로 p개의 slice가 어떤 조합으로 손실돼도 복구된다.
  - O(n log n)이다.
  - 구현별 (n, p) 제약은 다음과 같다. 당장은 n ≤ 32768, p ≤ n을 권고한다.
    - Leopard-RS(C++, BSD-3-Clause): 원본 + 복구 ≤ 65536이고, 복구 ≤ 원본이다. 2017년 README 기준이다. 조각 크기는 64바이트의 배수여야 한다. AVX2 단일 코어 인코딩이 1.2 GB/s 수준이다.
    - `reed-solomon-simd`(Rust, MIT AND BSD-3-Clause, 3.1.0): 원본과 복구가 각각 1–32768이면 아무 조합이나 된다. 한쪽이 32768을 넘으면 다른 쪽이 2ⁿ 이하일 때 2¹⁶ − 2ⁿ까지 허용된다(예: 61440 : 4096). 조각 크기는 짝수이면 된다. Ryzen 5 3600 단일 코어(AVX2), 1 KiB 조각에서 32 : 32는 10.2 GiB/s, 1024 : 1024는 4.8 GiB/s, 32768 : 32768은 1.6 GiB/s로 인코딩한다(원본과 복구 합계 기준). 1024 : 1024 디코딩은 1.3–1.4 GiB/s이다.
  > fountain code는 MDS가 아니고, RaptorQ에는 특허 문제가 있다.
- **인코딩 메모리는 R이 아니라 O(p·S)이다.** Leopard 계열 high-rate 인코더는 data shard를 m개(p 이상인 가장 작은 2의 거듭제곱)씩 IFFT해 작업 버퍼에 누적하고, 끝에 FFT를 한 번 한다. 그래서 보호 범위를 통째로 들고 있을 필요가 없고, 스트리밍 compressor도 R을 크게 잡을 수 있다. *(확인 필요, §8의 R6)*
  > Cauchy 행렬을 그대로 곱하는 방식도 메모리는 O(p·S)이지만, 입력 심볼마다 곱셈이 p번이라 p가 수백이면 너무 느리다.
- 확인할 것(R6):
  - `reed-solomon-simd`가 위의 누적을 API로 노출하는지, 아니면 모든 원본을 버퍼링하는지.
  - 평가점의 정확한 배치. Leopard는 parity를 0..m에, data를 m 이후에 두는 것으로 알고 있다.
  - 심볼의 바이트 배치. Leopard는 64바이트마다 심볼 32개의 하위 바이트와 상위 바이트를 따로 모아 둔다. 명세가 이것을 그대로 정의할지, 연속한 u16le로 정의하고 구현이 변환할지 정한다. S가 4096의 배수이므로 어느 쪽이든 64바이트 정렬은 맞는다.
  - (n, p)의 허용 조합을 명세에 어떻게 적을지.

#### 크기와 interleave

- 버스트 하나는 slice를 최대 ⌈L/S⌉ + 1개 지운다. 보호 범위 R, parity 비율 r이면 범위 안에서 견디는 버스트는 대략 **L ≤ r·R − S**이고, 흩어진 E1은 p개까지 견딘다. E1 하나가 parity를 S만큼 소모한다.
- 예를 들어 R = 1 GiB, S = 64 KiB이면 n = 16384이다. r = 1%이면 p = 164, parity는 10.25 MiB이다. 버스트는 약 10 MiB(E2)까지 견디고, 인코딩 메모리는 m = 256으로 약 32 MiB이다.
- E3을 노리면 R = 16 GiB, S = 512 KiB(n = 32768)로 잡는다. r = 1%이면 약 160 MiB이다.
- interleave(slice k를 set k mod G에 배정)는 두지 않는다.
  > interleave를 해도 같은 총 parity로 견디는 버스트 길이는 같다(G·p·S). interleave가 버는 것은 shard 수 상한 아래에서 S를 줄이는 것뿐이고, 그 대가로 전체가 MDS가 아니게 된다. Schroeder 2010(§2.1)이 interleaved parity가 예상보다 약하다고 보인 것과 같은 문제이다. S를 줄이고 싶으면 R을 줄인다.

#### 배치

- compressor는 보통 고정 크기 R마다 범위를 닫는다. 범위의 끝은 블록 경계와 관계없다. 그 set의 recovery block은 `range_end` + D 뒤의 첫 블록 경계에 둔다. 이러면 버스트 하나가 범위와 그 parity를 함께 지우지 않는다.
- data block이 매우 길면 그동안 닫힌 범위들의 parity(범위마다 약 r·R)가 쌓인다. compressor는 이것을 임시 파일로 내보내거나, 범위를 그 블록의 끝까지 늘리고 S를 키워 n을 상한 안에 둔다. 포맷은 둘 다 허용한다.
- 마지막 범위의 recovery block은 D 거리를 지킬 수 없다. 마지막 metadata block의 mirror와 같은 처지이고, segment 주기(§5.1)로 손실을 묶는다.
- 끝부분 truncation(E4)은 마지막 범위와 그 parity를 함께 지운다. parity가 항상 범위 뒤에 있으므로 이것은 구조적으로 피할 수 없다.

#### append

- 새 segment를 추가해도 기존 범위는 모두 옛 `archive_end` 앞에 있으므로 그대로 유효하다. 새 set은 새로 쓴 바이트를 덮는다.
- 새 set의 범위를 옛 마지막 범위의 끝에서 시작하면, 옛 꼬리에 있던 보호받지 못한 recovery block과 mirror block까지 덮을 수 있다.
  > 처음 제안은 RS의 선형성을 이용해 기존 parity를 제자리에서 갱신하는 옵션을 두었다. 범위가 분리되면 필요가 없고, crash 중의 parity 손실과 parity pollution(§2.1의 Krioukov 2008)의 위험만 남으므로 뺐다.

### 5.5 암호화 [잠정]

명세 §Encrypted archives에 반영했다. 헤더 필드의 사본(mirrored header)을 ciphertext 청크 사이에 두고, mirror block은 평문 안에 둔다. 아래는 명세에 없는 근거와 기각한 대안이다.

- age 청크가 손상되면 그 청크(64 KiB) 전체의 인증이 실패한다. 다만 손상은 erasure이고 오프셋이 보존되므로(§2.2), 실패한 청크를 건너뛰며 복호하면 평문 오프셋도 그대로이다. 그러면 mirror의 `target_offset`이 그대로 작동한다. 표준 age 구현은 첫 실패에서 멈추므로, 이 복호 절차는 repair 절(§5.6)에서 정의한다.
- 사본에는 payload nonce가 반드시 들어가야 한다. 처음 초안은 age 헤더(81바이트)만 복제했는데, nonce는 헤더 바로 뒤에 있어 앞부분 손상에 함께 지워지므로 사본이 쓸모없어진다. 그래서 nonce를 envelope 필드로 옮겼다.
  > `ENCRYPTED_HEADER_MAGIC` 쪽도 바이트 배치는 바뀌지 않는다. 서술만 "payload의 첫 16바이트"에서 envelope 필드로 바뀌었다.
- 사본의 위치는 오프셋이 아니라 인덱스로 기록한다. 사본이 정해진 위치에 없으면 어차피 청크 배치를 알 수 없으므로, 임의 위치를 표현할 수 있는 오프셋은 쓸모없는 값만 늘린다. 모든 필드가 고정 폭이므로 사본 하나로 archive_start와 모든 청크 위치가 닫힌 식으로 나온다.
- 간격은 1, 2, 4, …, 64청크로 늘리다가 64청크에서 멈춘다. 64청크는 평문 4 MiB로, §5.2의 거리 D와 같다.
  > 무한히 두 배로 늘리면 앞부분 손상에는 강하지만(앞쪽 L이 날아가도 다음 사본이 대략 2L 안에 있다), 마지막 사본이 대략 파일 중간에 있게 된다. 앞부분 손상과 truncation이 겹치면 살아남은 사본이 없을 수 있다. 사본은 117바이트라서 개수를 아낄 이유가 없다.
  > ext2/3/4의 sparse_super(0, 1, 3ⁱ, 5ʲ, 7ᵏ번째 block group)도 검토했다. 계산은 쉽지만(소인수분해의 유일성 때문에 1 말고는 겹치지 않는다) 같은 이유로 맞지 않는다. 그 배치는 superblock 사본이 비쌀 때를 위한 것이다.
  > 상한을 D에 묶은 이유: 버스트 하나가 mirror와 그 원본을 함께 지우지 않게 하려는 거리가 D이므로, 같은 버스트가 사본을 연달아 두 개 지우지 않게 하는 데도 같은 거리가 맞다. 처음에는 16청크(약 1 MiB)였으나, 근거가 없는 별도의 숫자라서 D에 맞췄다. D가 R1의 측정으로 바뀌면 상한도 함께 바뀐다.
- 사본을 아카이브마다 구별하는 필드는 두지 않는다. salt와 nonce가 무작위라서 97바이트 자체가 아카이브 ID 역할을 한다. 비암호화 mirror의 `archive_offset`이 막는 중첩 아카이브 문제도 여기서는 생기지 않는다. 안쪽 아카이브의 평문은 암호화되어 보이지 않고, 비암호화 아카이브 안에 저장된 암호화 아카이브는 필드가 다르다.
- magic은 두 개이다.
  - `ENCRYPTED_MIRRORED_HEADER_MAGIC`은 사본이 있는 형식을 표시한다. 비암호화 아카이브에서 mirror가 선택 사항인 것처럼 사본도 선택 사항이므로, 기존 `ENCRYPTED_HEADER_MAGIC`을 유지한다. 마지막 2바이트를 type prefix와 별개인 버전 필드로 서술하지 않은 것은 확장 가능하다는 인상을 주지 않기 위해서이다. 그래서 모르는 값은 unsupported가 아니라 invalid archive가 된다.
  - `ENCRYPTED_MIRROR_MAGIC`은 스캔용이다. `MIRROR_MAGIC`을 공유하면 비암호화 아카이브 안에 암호화 아카이브가 저장된 경우 두 스캐너가 서로의 후보를 집는다. 값의 조건은 §5.2와 같다. `0A`가 들어 있어 텍스트 모드 전송으로 인한 손상도 드러난다.
- `ENCRYPTED_MIRRORED_HEADER_MAGIC` 형식에서는 원본 헤더 끝에도 XXH64를 둔다. 복구에는 필요 없다(검증된 사본과 다르면 원본이 손상된 것이다). 대신 헤더 손상을 scrypt 전에 invalid archive로 보고한다. checksum이 없으면 salt, file key, work factor의 손상이 어떤 패스프레이즈로도 decryption failure가 되어 패스프레이즈 오류와 구별되지 않고, work factor가 커지는 쪽으로 깨지면 scrypt가 한없이 돈다. 사본이 없는 한 청크짜리 아카이브에서도 감지된다.
- 프레임은 짝수로 정렬하지 않는다. 원본 헤더는 113바이트(`ENCRYPTED_HEADER_MAGIC` 형식은 105바이트), 사본은 117바이트이다. GF(2¹⁶) RS(§5.4)는 복호한 평문 위에서 동작하므로 ciphertext의 정렬은 필요 없다.
- 평문 안 mirror의 거리 규칙은 청크 단위로 바꿨다. 4 KiB 단위 하나가 인접한 두 청크에 걸칠 수 있으므로, 원본과 사본 사이에 온전한 청크가 하나 이상 있어야 한다.
- 남는 한계:
  - 4 KiB 손상이 평문 64–128 KiB 손실로 커진다. 작은 파일이 많은 구간에서는 "손상된 바이트가 속한 parcel만 잃는다"는 E1 목표를 만족하지 못한다. age 청크 크기가 고정이라 안쪽에서는 해결할 수 없다.
  - 키 없이는 손상 감지도 복구도 할 수 없다(mirrored header 자체의 복원은 예외). 키 없는 scrub이 필요해지면 ciphertext 해시 인덱스나 parity를 non-critical 프레임으로 청크 사이에 넣는 확장을 검토한다. ciphertext의 해시는 ciphertext 이상의 정보를 드러내지 않는다.
  - 복원한 청크를 원래 file key와 nonce로 다시 암호화하면, 평문이 원본과 완전히 같지 않은 한 nonce 재사용이 된다(명세의 노트). repair 결과는 평문으로 내거나 새 file key로 다시 암호화한다.

### 5.6 repair 절차 [잠정]

#### 입력 모델

repair의 입력은 바이트열이 아니라, 위치마다 다음 상태 중 하나가 붙은 열이다.

- **인증된 바이트:** 암호화 아카이브에서 인증에 성공한 청크의 평문이다.
- **검증되지 않은 바이트:** 읽기에 성공했지만 아무것도 보증하지 않는 바이트이다. 비암호화 아카이브의 모든 바이트와, 인증에 실패한 청크에서 읽기에 성공한 부분을 키스트림으로 복호한 바이트(미인증 바이트)가 여기에 속한다.
- **erasure(⊥):** 읽지 못했거나 복원할 수 없는 바이트이다.

> placeholder 바이트 값을 정하지 않고 바이트열 밖의 표지(⊥)를 쓰는 이유: 평문에는 모든 바이트 값이 정상적으로 나오므로 값 하나를 placeholder로 정할 수 없다. 또 ⊥ 자리를 0 같은 값으로 채워 일반 손상처럼 다루면, checksum이 없는 leaf 블록에서는 채운 값이 그대로 파일 내용이 된다. 그래서 위치 정보를 끝까지 넘겨야 한다.
>
> 미인증 바이트를 비암호화 아카이브의 바이트와 같은 상태로 두는 이유: ChaCha20은 스트림 암호라서 인증에 실패한 청크도 멀쩡한 바이트는 바르게 복호된다. 그 바이트가 보증받지 못하는 정도는 비암호화 아카이브의 바이트와 같다. 둘 다 블록 checksum으로만 검증된다. 이것을 쓰면 위치를 아는 손상(LSE 등)에서는 손실이 64 KiB 청크가 아니라 실제 erasure 범위로 줄어든다(§5.5의 손상 증폭). 대신 암호화가 약속한 무결성은 그 청크에서 사라진다. ChaCha20은 malleable하므로 위조된 입력이라면 평문의 비트를 마음대로 뒤집을 수 있다. 그래서 미인증 바이트는 repair 모드에서 명시적으로 켰을 때만 쓰고, 미인증 바이트에서 나온 결과에는 그렇다는 표시를 붙인다(§6의 "복구는 보안 기능이 아니다"와 같은 노선).

#### 절차

0. (암호화 아카이브) 복호한다. 이 단계는 ⊥가 섞인 ciphertext를 받아 위 상태가 붙은 평문을 낸다.
   1. 헤더 필드를 얻는다. 원본 헤더의 `header_checksum`이 맞으면 원본을 쓰고, 아니면 `ENCRYPTED_MIRROR_MAGIC`을 스캔해 checksum이 맞는 사본을 쓴다. 사본의 인덱스와 위치에서 archive_start를 정한다. 어느 쪽도 없으면 복호할 수 없다.
   2. 청크 위치는 명세의 닫힌 식으로 정한다. 손상이 shift를 일으키지 않는다는 전제(§2.2의 E6 제외) 아래에서 정확하다.
   3. 인증에 성공한 청크는 인증된 바이트가 된다. 실패한 청크에서 ⊥가 아닌 바이트는 키스트림으로 복호해 미인증 바이트로 두고(명시적으로 켰을 때), ⊥는 ⊥로 둔다. 미인증 바이트를 쓰지 않으면 청크 전체가 ⊥이다.
   4. final 플래그는 청크의 물리적 위치로 정한다. 다만 마지막 청크가 잘렸을 때처럼 모호하면 두 경우를 모두 복호해 보고, 평문의 블록 구조와 checksum으로 고른다.
1. `mirror_magic`과 `recovery_magic`을 스캔하고, 각 블록의 `mirror_checksum`이나 `recovery_checksum`을 검증한다. `archive_offset`이 archive_start와 맞지 않는 블록은 중첩 아카이브의 것이므로 버린다(§5.2의 찾기). 이 스캔에는 ⊥가 아닌 바이트만 쓴다.
2. mirror마다 원본을 사본과 비교한다. 원본에 ⊥가 있거나 사본과 다르면 사본으로 덮어쓴다. 덮어쓴 바이트는 사본의 checksum으로 검증되었으므로 인증된 바이트와 같은 정도로 신뢰한다. 비용이 싸고, 이후 RS 단계의 erasure를 줄여 준다. 구조(metadata 체인이나 앞방향 스캔)로 위치가 확인되지 않은 mirror는 1단계의 `archive_offset` 검사를 통과해야만 덮어쓰기에 쓴다.
3. recovery set이 있으면 검증된 recovery block을 set의 정체(§5.4)로 묶는다. set마다 ⊥와 slice 해시 불일치를 erasure로 표시하고, erasure가 살아 있는 parity shard 수 이하이면 RS로 복원한다. set은 서로의 recovery block을 보호하고 mirror 복원도 erasure를 줄이므로, 2단계와 3단계를 더 이상 복원되는 것이 없을 때까지 반복한다. 정상적인 배치에서는 범위가 뒤에 있는 set부터 한 번씩 돌면 끝난다.
4. footer가 깨졌으면 앞방향 스캔으로 재구성한다.
5. ⊥가 남지 않았으면 결과를 바이트열로 보고 `interpret an archive`에 넘긴다. 명세의 해석은 바뀌지 않는다. ⊥가 남았으면 strict 해석의 대상이 아니다. 그때는 salvage로 넘겨 ⊥를 읽지 않고 계산되는 부분만 낸다. 이것은 명세 §Partial access의 연장으로 정의할 수 있다.
6. (암호화 아카이브) 결과는 평문으로 내거나 새 file key로 다시 암호화한다(§5.5).

## 6. 범위 밖

- E5(장치나 파일 전체 손실): 별도 사본으로 대응한다.
- E6(shift): 필요해지면 rolling hash 기반 slice 재탐색을 따로 설계한다.
- 생성 시점의 RAM 비트 플립: 쓴 직후 검증으로 대응한다.
- 악의적 변조: 복구는 보안 기능이 아니다. 무결성과 인증은 암호화의 몫이다.

## 7. 열린 질문

1. ~~**mirror block 하나에 여러 원본을 담을지.**~~ 원본 하나만 담는다(명세 §Mirror block).
2. ~~**`target_offset`의 부호.**~~ 원본은 항상 mirror 앞에 온다(명세 §Mirror block).
3. **해시.** XXH64 하나로 통일할지, slice 인덱스에는 XXH3-128을 쓸지. 상대는 무작위 손상뿐이므로 64비트로 충분하다고 본다.
4. ~~**"무시해도 되는 블록"의 명세 용어.**~~ leading digit(두 자리로 채운 첫 16진 자리)이 A–F인 non-critical 타입으로 정했다. 모르는 타입은 ignored block이 된다(명세 §Reserved block types).
5. ~~**encrypted archive 안의 mirror 허용 여부.**~~ 허용한다. 헤더 필드는 mirrored header로 따로 보호한다(명세 §Encrypted archives, §5.5).
6. **스크러빙.** 필요한 여유분은 "다음 검증 전까지 쌓일 손상"이 정한다. 그래서 `verify` 명령과 권장 주기를 도구 쪽에서 함께 설계해야 한다.
7. **바이트 상태를 명세에 넣을지.** §5.6의 세 상태(인증됨, 검증되지 않음, ⊥)는 지금 이 문서에만 있다. 명세의 Repair 절(S3)을 쓸 때 입력 모델로 넣을지, ⊥만 넣고 미인증 바이트는 구현의 선택으로 남길지 정한다. 미인증 바이트를 기본으로 켤지도 함께 정한다.
8. **암호화 아카이브의 RS 위치.** §5.4의 RS를 복호한 평문 위에 두면, erasure 단위가 청크(64 KiB)가 되어 흩어진 E1 손상이 parity를 16–32배 빨리 소모하고, 키가 없으면 repair할 수 없다. ciphertext 위(청크 사이의 non-critical 프레임)에 두면 두 문제가 모두 사라지지만, 비암호화 쪽과 RS 구조가 갈라지고 프레임 정렬(§5.5)을 다시 봐야 한다. §5.6의 미인증 바이트는 위치를 아는 손상에 한해 첫 번째 문제를 줄인다. 또 S를 64 KiB의 배수로 잡으면 slice가 청크와 정렬되므로(§5.4의 slice 격자) 청크 하나의 손실이 slice 하나만 소모한다. 이때 첫 번째 문제는 S ≥ 64 KiB의 비용으로 바뀌고, 키 없는 repair만 남는다.

## 8. Action items

### 조사 (R)

- [x] **R1.** Schroeder 등 FAST 2010의 LSE 버스트 길이 분포와 intra-disk redundancy 평가 결과를 확인하고, §2.2의 E2 범위와 §5.2의 D 기본값을 맞춘다.
- [x] **R2.** Bairavasundaram 등 SIGMETRICS 2007·FAST 2008, Krioukov 등 FAST 2008, Schroeder 등 FAST 2016의 수치를 확인해 §2.1을 보강한다.
- [x] **R3.** 현행 SSD의 erase block과 superblock 크기를 확인해 E2/E3의 경계를 검증한다. (176층 이후 제품의 데이터시트 수치는 찾지 못했다. erase block은 E3로 옮겼다.)
- [x] **R4.** Par2 행렬이 MDS가 아니라는 지적과, Par3 draft가 다른 포맷 안에 복구 데이터를 넣는 경우를 다루는지 확인한다.
- [x] **R5.** Leopard 계열 구현(C++ leopard, Rust `reed-solomon-simd` 등)의 slice 수와 비율 제약, 라이선스, 처리 속도를 확인한다.
- [ ] **R6.** additive-FFT RS 구현(Leopard, `reed-solomon-simd`)의 평가점 배치, 심볼 바이트 배치, (n, p) 제약을 확인하고, m개 단위 누적으로 O(p·S) 메모리 스트리밍 인코딩이 가능한지와 그것을 API로 쓸 수 있는지 확인한다(§5.4의 부호).

### 측정 (M)

- [ ] **M1.** 기존 말뭉치(`scripts/zstd-dict-*`의 cargo, rustdoc, pystdlib 등)에서 metadata, dictionary, module, resource, formula block이 아카이브 크기에서 차지하는 비율을 잰다. mirror 오버헤드의 근거가 된다.
- [ ] **M2.** segment 주기(예: 16, 64, 256 MiB)별 metadata 오버헤드와 디렉토리 반복 비용을 재서 §5.1의 기본값을 정한다.
- [ ] **M3.** (§5.4를 진행할 경우) FFT RS의 인코딩과 디코딩 처리량을 재서 압축 속도 대비 부담을 확인한다.

### 명세 (S)

- [x] **S1.** "무시해도 되는 블록" 범주를 정의한다(§7의 4번). (non-critical 타입으로 정했다. spacer는 0으로 채워진 구멍이 spacer 블록으로 파싱되도록 00–03에 그대로 둔다.)
- [x] **S2.** mirror block 절을 추가한다. 구조와 배치 SHOULD를 포함한다. (`MIRROR_MAGIC` 값 확정, `archive_offset` 추가함)
- [ ] **S3.** 해석과 분리된 "Repair" 절을 추가한다(§4의 1번, §5.6). 암호화 아카이브의 복호 단계와 ⊥가 남은 경우의 salvage를 포함한다(§7의 7번).
- [ ] **S4.** 비규범 노트를 추가한다. 내용은 오류 모델 요약, E5가 범위 밖이라는 점, segment 주기 권고, checksum 있는 블록 타입을 기본값으로 쓰라는 권고이다.
- [ ] **S5.** 명세 §Block types의 표에 mirror 타입(반영함)과, §5.4를 진행하면 recovery 타입(B0–B7 등 non-critical)을 반영한다.
- [ ] **S6.** (§5.4를 진행할 경우) recovery block 절과 부호의 완전한 정의를 추가한다.
- [x] **S7.** encrypted envelope의 복구 구조를 정한다(§5.5). (trailer 대신 청크 사이의 mirrored header로 정했다. 실패한 청크를 건너뛰는 복호는 S3에서 정의한다.)

### 구현 (I)

- [ ] **I1.** E1–E4를 재현하는 fault injection 하네스를 만든다. 오류 모델을 그대로 시험 생성기로 옮기는 것이다. 4 KiB 정렬 단위 손상, 버스트, 0 채움, truncation을 다루고, 손상 위치는 무작위와 "중요 구조 조준" 두 방식으로 둔다.
- [ ] **I2.** compressor에 mirror block 생성과 segment 주기 옵션을 넣는다.
- [ ] **I3.** repair 도구를 만든다. 스캔, mirror 복원, footer 재구성, salvage 추출을 포함한다.
- [ ] **I4.** `verify` 명령을 만든다. checksum, mirror와 원본의 일치, slice 해시를 모두 검사한다.
- [ ] **I5.** I1로 "손상된 바이트가 속한 parcel만 잃는다"는 목표를 E1과 E2(D 이상 아카이브)에 대해 검증한다.
- [ ] **I6.** (선택) §5.4의 recovery set을 구현하고, I1로 L ≤ r·R − S 한계를 검증한다.

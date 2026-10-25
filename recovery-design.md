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
  - §3.1 Block: 블록의 일반 구조와 짝수 길이 padding.
  - §3.1.2 Spacer block: 무시해도 되는 블록의 첫 선례.
  - §3.1.7 Reserved blocks: 첫 16진 자리가 A–F인 non-critical 타입은 모르면 무시한다(ignored block).
  - §3.1.5 Content blocks: segment와 parcel, 그리고 "모든 content block은 뒤따르는 metadata block이 기술해야 한다"는 규칙.
  - §3.2.1 Unencrypted Archive: footer, `metadata_backward_offset`, u16 `footer_size`.
  - §3.2.2 Encrypted Archive: age 기반 envelope. 안쪽에서는 spacer와 trailing byte를 금지한다.
  - §4.6 Partial access: 부분 결과와 손상 감지 의무의 범위.
  - §7 Block index: 예약된 타입의 critical 여부는 첫 16진 자리로 정해진다.

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
| **Par3** (2022-01-28 alpha draft, 미확정) | BLAKE3(하위 16바이트), 임의의 선형 부호. 행렬 패킷은 Cauchy(곧 RS), sparse random, explicit(LDPC용) 세 가지이다. Galois field는 8비트의 배수이면 된다. FFT 부호는 정의하지 않는다. | 부호를 고를 수 있다. **"Par inside ZIP"을 명시적으로 다룬다.** ZIP의 데이터와 central directory 사이에 Par3 패킷을 넣고, 패킷이 들어갈 자리를 0으로 간주한 checksum으로 파일 자신을 보호한다. 패킷 자신은 보호하지 못한다. §5.4의 "recovery 영역을 0으로 간주"와 같은 발상이다. |
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
  > 이 설계의 교훈: 복구 단계는 손상된 원본으로 복구 데이터를 갱신하면 안 된다. repair가 원본을 덮어쓰기 전에 사본 쪽 checksum을 검증하는 것(§5.2)과 append 때의 parity 제자리 갱신(§5.4)이 여기에 해당한다.
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

- 손상 단위는 **파일 오프셋 기준 4 KiB 정렬**이다. archive_start 기준이 아니다. 아카이브가 파일 처음에서 시작하지 않을 수도 있기 때문이다(§3.2.1).
- 오류끼리는 독립이 아니라 **공간적으로 상관**되어 있다. 따라서 핵심 방어는 하나의 손상이 여러 사본을 동시에 덮치지 않도록 멀리 떨어뜨리는 것이다.

> E6은 저장 장치에서 거의 일어나지 않고, 주로 버그 있는 도구나 텍스트 모드 전송에서 생긴다. Par2는 rolling CRC로 어긋난 slice를 다시 찾는데, 이것이 E6 대응에 해당한다. 이 기능은 v1 범위 밖이다.

## 3. 현재 명세의 유불리

유리한 점:

- **모든 블록 길이가 짝수이다**(block padding). GF(2¹⁶) 심볼(2바이트)과 그대로 맞는다.
- **Spacer처럼 무시해도 되는 블록이라는 개념이 이미 있다.** recovery 관련 블록을 같은 지위로 두면, 복구 기능을 모르는 디코더도 아카이브를 그대로 읽는다. 명세는 non-critical 타입(첫 16진 자리 A–F)을 모르면 무시하므로, recovery 블록을 그 구간에 두면 된다.
- **segment는 self-contained이고 parcel 구조가 있으며, 블록 간에 공유되는 압축 문맥이 없다**(dictionary 제외). 복구에 실패해도 손상 범위 밖의 파일은 살릴 수 있고, §4.6 Partial access가 이미 이를 허용한다.
- **segment를 찾는 경로가 둘이다.** 앞방향 스캔과 footer에서 시작하는 metadata 역방향 체인이다.
- **footer는 나머지에서 재구성할 수 있다.** `size_hint`는 길이에서, `metadata_backward_offset`은 스캔에서 얻는다. 그래서 보호 범위에서 뺄 수 있다. header magic은 상수이고, size hint block은 footer와 중복이다.

불리한 점:

- **블록 헤더에 sync marker가 없다.** 대부분 1바이트 leb128 type과 size뿐이라, size 필드 하나가 깨지면 앞방향 스캔이 탈선한다.
- **기존 checksum은 디코딩된 데이터에 대한 것이다.**
  - 저장 바이트의 손상 위치를 찾으려면 압축을 풀어 봐야 한다.
- **`footer_size`가 u16이다.** 해시 목록 같은 인덱스를 둘 곳이 못 된다.
- **Encrypted archive에는 확장할 자리가 없다.** payload 뒤의 trailing byte가 금지되어 있고, 키 없이는 age payload가 어디서 끝나는지 알 수 없다.
- **metadata는 segment 끝에, footer는 아카이브 끝에 있다.** 가장 흔한 손상인 E4가 가장 중요한 구조를 먼저 지운다(§5.1).

## 4. 설계 원칙 [제안]

1. **복구는 해석(semantics) 밖의 전처리로 정의한다.** repair는 손상된 바이트열을 받아, recovery 데이터로 검증되는 바이트열을 내놓는 과정이다. 명세의 `interpret an archive`와 "invalid archive는 입력만의 성질이다"라는 원칙은 바뀌지 않는다.
2. **recovery 관련 블록은 무시해도 되는 블록이다.** spacer처럼 블록 사이 어디에나 올 수 있고, segment에 속하지 않으며, 마지막 metadata block 뒤에 와도 된다.
3. **오류는 erasure로만 다룬다.** 손상 위치는 저장 바이트에 대한 해시로 찾는다.
4. **복구 데이터는 스스로 찾아질 수 있어야 한다.** 블록마다 sync magic과 자기 해시를 두어, header나 footer, 체인이 깨져도 스캔으로 찾을 수 있게 한다.
5. **부호와 배치 규칙은 명세에 완전히 고정한다.** 행렬과 평가점까지 고정해, 모든 구현이 같은 결과를 내게 한다.

## 5. 단계별 설계

우선순위는 §5.1 > §5.2 > §5.4 순이고, §5.3은 선택이다.

### 5.1 segment 주기 (E4) [제안]

metadata는 데이터를 다 쓴 뒤에야 만들어진다. 그래서 스트리밍 인코더는 마지막 segment의 metadata 사본을 앞쪽에 둘 수 없고, truncation이 일어나면 마지막 segment는 metadata를 반드시 잃는다.

- compressor는 N MiB(또는 N초)마다 segment를 닫는다. 그러면 truncation 손실은 **아직 닫히지 않은 마지막 segment 하나로 묶인다**.
- 명세에는 비규범 권고로 둔다. 백업 도구가 기본값을 정한다.
- metadata를 잃은 마지막 segment의 leaf block도 이름 없는 파일로는 추출할 수 있다(photorec식 salvage).

> 대가도 있다. segment가 많아질수록 metadata 오버헤드가 늘고, 디렉토리가 segment마다 다시 나타난다(명세 §4.2.2 Merging segments). 기본 주기는 실측으로 정해야 한다(§8의 M2).

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

명세 §3.1.6 Mirror block에 반영했다. 아래는 명세에 없는 근거와 기각한 대안이다.

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

명세 §3.1.6 Mirror block에 반영했다(E1, E2의 거리 SHOULD와 블록 종류별 배치 요령). 거리 D의 값은 명세에서도 잠정이다. 근거는 다음과 같다.

길이 L인 버스트는, 원본과 사본의 거리 D가 L보다 짧을 때만 둘을 함께 지운다.

- **E3:** 거리를 고정값이 아니라 아카이브 크기의 비율로 잡으면 노려볼 수 있다. 다만 append 구조와 충돌하기 쉬우므로 best-effort로 둔다.

#### 찾기

마지막 metadata가 깨지면 체인의 출발점이 사라진다. 그때는 `mirror_magic` 스캔에 의존한다. repair 모드에서 아카이브 전체를 한 번 읽는 비용은 감수한다. 후보의 확정 절차와 archive_start 추정은 명세 §3.1.6 Mirror block에 반영했다.

> archive_start 추정에 다수결을 쓰지 않는 이유: 안쪽 아카이브의 mirror가 바깥 것보다 많을 수 있다.

### 5.3 slice 해시 인덱스: 감지 전용 [제안, 선택]

- 저장 바이트를 `slice_size` 단위(2의 거듭제곱, 4 KiB 이상)로 나누고, slice마다 XXH64를 기록한다.
- parity 없이 인덱스만 두면 오버헤드가 0.01% 수준이고, 손상 위치를 정확히 짚을 수 있다.
- §5.2가 있으면 각 파일의 checksum으로 대부분 대체되므로 우선순위는 낮다. 다만 §5.4를 쓴다면 필수이다.

### 5.4 전체 RS parity (E3) [제안, 선택]

파일 내용까지 복구하려는 사용자를 위한 옵션이다. §5.2가 이미 있으므로, 이 단계의 복잡도는 순수하게 내용 보호의 비용이 된다.

- **블록 타입:** non-critical 구간, 예를 들어 B0–B3을 recovery block으로 쓴다. 공통 앞부분은 `recovery_magic`, `self_hash`, `set_id`, `archive_offset`(archive_start로부터 이 블록까지의 거리), `kind`(index 또는 parity)이다.
  - `set_id`는 파라미터의 해시이다. 세대(generation)와 아카이브를 구분한다.
  - `archive_offset`이 있으면 header가 깨져도 slice 격자를 복원할 수 있다. 중첩 아카이브의 recovery block을 가려내는 데도 mirror와 같은 방식으로 쓴다(§5.2의 찾기).
  - `recovery_magic`의 길이와 값에 대해서는 §5.2의 `mirror_magic` 분석이 그대로 적용된다.
- **index 패킷:** `slice_size`, 보호 길이, set 배치, 부호 ID, slice 해시 목록(§5.3)을 담는다. 최소 두 벌을 서로 떨어진 위치에 둔다(Par2의 main packet 중복과 같은 발상).
- **보호 범위:** `[archive_start, archive_end)`에서 recovery block과 mirror block 영역을 0으로 간주한다. 이러면 자기 자신을 보호하는 순환이 없어진다. footer는 보호하지 않고 재구성한다.
- **부호:** GF(2¹⁶) 위의 additive-FFT 기반 RS(Lin–Chung–Han 계열, Leopard-RS가 대표 구현)를 제안한다.
  - MDS이므로 M개의 slice가 어떤 조합으로 손실돼도 복구된다.
  - O(n log n)이다.
  - 총 slice 수는 65536개로 제한된다. 원본과 복구의 비율 제약은 구현마다 다르다.
    - Leopard-RS(C++, BSD-3-Clause): 원본 + 복구 ≤ 65536이고, 복구 ≤ 원본이다. 2017년 README 기준이다. 조각 크기는 64바이트의 배수여야 한다. AVX2 단일 코어 인코딩이 1.2 GB/s 수준이다.
    - `reed-solomon-simd`(Rust, MIT AND BSD-3-Clause, 3.1.0): 원본과 복구가 각각 1–32768이면 아무 조합이나 된다. 한쪽이 32768을 넘으면 다른 쪽이 2ⁿ 이하일 때 2¹⁶ − 2ⁿ까지 허용된다(예: 61440 : 4096). 조각 크기는 짝수이면 된다. Ryzen 5 3600 단일 코어(AVX2), 1 KiB 조각에서 32 : 32는 10.2 GiB/s, 1024 : 1024는 4.8 GiB/s, 32768 : 32768은 1.6 GiB/s로 인코딩한다(원본과 복구 합계 기준). 1024 : 1024 디코딩은 1.3–1.4 GiB/s이다.
    > 따라서 set 하나의 slice 수는 원본과 복구를 합쳐 65536개 이하로 두고, 복구 비율이 50%를 넘지 않게 하면 두 구현 모두에서 동작한다. 일반적인 parity 비율은 몇 %이므로 제약이 되지 않는다.
  > fountain code는 MDS가 아니고, RaptorQ에는 특허 문제가 있다.
- **대용량과 interleave:**
  - set 하나로 1 TB를 덮으면 slice가 16 MB가 되어, 섹터 하나만 깨져도 16 MB를 잃는다.
  - 그래서 G개의 set으로 나누고, slice k를 set (k mod G)에 배정한다.
  - slice 크기 S, set당 parity p개일 때 견딜 수 있는 버스트 길이는 대략 **L ≤ G·p·S**이다. 예를 들어 S = 64 KiB, p = 16, G = 64이면 약 64 MiB이다.
  > G를 키우면 같은 총 parity가 여러 set으로 쪼개진다. 흩어진 E1이 한 set에 몰리면 그 set만 실패할 수 있지만, 확률이 낮아 실용적으로는 무시할 만하다.
- **배치:** 기본은 footer 앞이다. 끝부분 truncation에는 데이터 꼬리와 parity가 함께 날아가므로, dvdisaster처럼 분산 배치하는 옵션을 둔다. 스트리밍 인코더라면 set을 연속 범위로 잡고, 각 set의 parity를 다음 set 뒤에 지연 배치한다.
- **append:** 새 segment를 추가해도 기존 바이트는 변하지 않는다(footer는 보호 범위 밖). 따라서 기존 parity는 계속 유효하다.
  - 기본은 새 범위만 덮는 새 세대의 set을 추가하는 것이다. 이 경우 보호력은 세대 범위 안에서만 작동한다.
  - RS는 선형이므로, 기존 데이터를 다시 읽지 않고 기존 parity에 새 데이터의 기여분을 더해 제자리에서 갱신할 수도 있다. 이 옵션은 spacer의 원래 용도와도 맞는다. 단, 쓰는 도중 crash가 나면 그 parity를 잃는다(self_hash 불일치로 버려질 뿐, 데이터는 무사하다).

### 5.5 암호화 [제안]

- age 청크가 손상되면 그 청크(64 KiB) 전체의 인증이 실패한다. 따라서 복구 데이터는 **ciphertext 바깥**에 두어야 한다. 바깥에 두어도 ciphertext 이상의 정보는 드러나지 않는다.
- 이를 위해 envelope 확장이 필요하다. 예를 들어 payload 뒤에 footer식 trailer(끝에 고정 크기 마커와 크기 사본)를 허용한다. 현재의 "trailing bytes 금지" 규칙을 바꿔야 한다.
- 안쪽(평문)에 둔 mirror도, 인증에 실패한 청크를 건너뛰는 비표준 age 복호기가 있으면 쓸모가 있다. 하지만 표준 라이브러리는 첫 실패에서 중단한다.

### 5.6 repair 절차 [제안]

1. `mirror_magic`과 `recovery_magic`을 스캔하고, 각 블록의 `mirror_checksum`이나 `self_hash`를 검증한다. `archive_offset`이 archive_start와 맞지 않는 블록은 중첩 아카이브의 것이므로 버린다(§5.2의 찾기).
2. mirror마다 원본을 사본과 비교하고, 다르면 사본으로 덮어쓴다. 비용이 싸고, 이후 RS 단계의 erasure를 줄여 준다. 구조(metadata 체인이나 앞방향 스캔)로 위치가 확인되지 않은 mirror는 1단계의 `archive_offset` 검사를 통과해야만 덮어쓰기에 쓴다.
3. recovery set이 있으면 slice 해시로 erasure를 표시하고 RS로 복원한다.
4. footer가 깨졌으면 앞방향 스캔으로 재구성한다.
5. 결과 바이트열을 `interpret an archive`에 넘긴다. 복원할 수 없는 부분은 §4.6 Partial access의 salvage로 넘긴다.

## 6. 범위 밖

- E5(장치나 파일 전체 손실): 별도 사본으로 대응한다.
- E6(shift): 필요해지면 rolling hash 기반 slice 재탐색을 따로 설계한다.
- 생성 시점의 RAM 비트 플립: 쓴 직후 검증으로 대응한다.
- 악의적 변조: 복구는 보안 기능이 아니다. 무결성과 인증은 암호화의 몫이다.

## 7. 열린 질문

1. ~~**mirror block 하나에 여러 원본을 담을지.**~~ 원본 하나만 담는다(명세 §3.1.6 Mirror block).
2. ~~**`target_offset`의 부호.**~~ 원본은 항상 mirror 앞에 온다(명세 §3.1.6 Mirror block).
3. **해시.** XXH64 하나로 통일할지, slice 인덱스에는 XXH3-128을 쓸지. 상대는 무작위 손상뿐이므로 64비트로 충분하다고 본다.
4. ~~**"무시해도 되는 블록"의 명세 용어.**~~ 첫 16진 자리가 A–F인 non-critical 타입으로 정했다. 모르는 타입은 ignored block이 된다(명세 §3.1.7 Reserved blocks).
5. **encrypted archive 안의 mirror 허용 여부.** 현재 안쪽에서는 spacer를 금지하며, mirror도 일단 금지했다.
6. **스크러빙.** 필요한 여유분은 "다음 검증 전까지 쌓일 손상"이 정한다. 그래서 `verify` 명령과 권장 주기를 도구 쪽에서 함께 설계해야 한다.

## 8. Action items

### 조사 (R)

- [x] **R1.** Schroeder 등 FAST 2010의 LSE 버스트 길이 분포와 intra-disk redundancy 평가 결과를 확인하고, §2.2의 E2 범위와 §5.2의 D 기본값을 맞춘다.
- [x] **R2.** Bairavasundaram 등 SIGMETRICS 2007·FAST 2008, Krioukov 등 FAST 2008, Schroeder 등 FAST 2016의 수치를 확인해 §2.1을 보강한다.
- [x] **R3.** 현행 SSD의 erase block과 superblock 크기를 확인해 E2/E3의 경계를 검증한다. (176층 이후 제품의 데이터시트 수치는 찾지 못했다. erase block은 E3로 옮겼다.)
- [x] **R4.** Par2 행렬이 MDS가 아니라는 지적과, Par3 draft가 다른 포맷 안에 복구 데이터를 넣는 경우를 다루는지 확인한다.
- [x] **R5.** Leopard 계열 구현(C++ leopard, Rust `reed-solomon-simd` 등)의 slice 수와 비율 제약, 라이선스, 처리 속도를 확인한다.

### 측정 (M)

- [ ] **M1.** 기존 말뭉치(`scripts/zstd-dict-*`의 cargo, rustdoc, pystdlib 등)에서 metadata, dictionary, module, resource, formula block이 아카이브 크기에서 차지하는 비율을 잰다. mirror 오버헤드의 근거가 된다.
- [ ] **M2.** segment 주기(예: 16, 64, 256 MiB)별 metadata 오버헤드와 디렉토리 반복 비용을 재서 §5.1의 기본값을 정한다.
- [ ] **M3.** (§5.4를 진행할 경우) FFT RS의 인코딩과 디코딩 처리량을 재서 압축 속도 대비 부담을 확인한다.

### 명세 (S)

- [x] **S1.** "무시해도 되는 블록" 범주를 정의한다(§7의 4번). (non-critical 타입으로 정했다. spacer는 0으로 채워진 구멍이 spacer 블록으로 파싱되도록 00–03에 그대로 둔다.)
- [x] **S2.** mirror block 절을 추가한다. 구조와 배치 SHOULD를 포함한다. (`MIRROR_MAGIC` 값 확정, `archive_offset` 추가함)
- [ ] **S3.** 해석과 분리된 "Repair" 절을 추가한다(§4의 1번, §5.6).
- [ ] **S4.** 비규범 노트를 추가한다. 내용은 오류 모델 요약, E5가 범위 밖이라는 점, segment 주기 권고, checksum 있는 블록 타입을 기본값으로 쓰라는 권고이다.
- [ ] **S5.** §7 Block index에 mirror 타입(반영함)과, §5.4를 진행하면 recovery 타입(B0–B3 등 non-critical)을 반영한다.
- [ ] **S6.** (§5.4를 진행할 경우) recovery block 절과 부호의 완전한 정의를 추가한다.
- [ ] **S7.** encrypted envelope의 trailer 확장안을 정한다(§5.5).

### 구현 (I)

- [ ] **I1.** E1–E4를 재현하는 fault injection 하네스를 만든다. 오류 모델을 그대로 시험 생성기로 옮기는 것이다. 4 KiB 정렬 단위 손상, 버스트, 0 채움, truncation을 다루고, 손상 위치는 무작위와 "중요 구조 조준" 두 방식으로 둔다.
- [ ] **I2.** compressor에 mirror block 생성과 segment 주기 옵션을 넣는다.
- [ ] **I3.** repair 도구를 만든다. 스캔, mirror 복원, footer 재구성, salvage 추출을 포함한다.
- [ ] **I4.** `verify` 명령을 만든다. checksum, mirror와 원본의 일치, slice 해시를 모두 검사한다.
- [ ] **I5.** I1로 "손상된 바이트가 속한 parcel만 잃는다"는 목표를 E1과 E2(D 이상 아카이브)에 대해 검증한다.
- [ ] **I6.** (선택) §5.4의 recovery set을 구현하고, I1로 L ≤ G·p·S 한계를 검증한다.

# Aqueous 오류 감지와 복구(recovery) 설계

아카이브를 백업에 쓰는 흔한 용례를 위해, Aqueous 포맷에 오류 감지와 복구를 넣는 설계를 기술한다.
FreeArc가 이 기능을 진지하게 고려한 것이 계기가 되었다.
본문은 현재 제안된 설계를 서술하고, 그렇게 정한 근거는 인용 블록에 둔다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다. 이 문서의 선행 사례와 실증 연구 서술은 기억에 기반하며, 이번에 원문을 다시 확인하지 않았다(§8의 R 항목).

관련 코드:

- `spec/index.bs`: 아카이브 포맷 명세 초안. 이 문서와 관련된 절은 다음과 같다.
  - §3.1 Block: 블록의 일반 구조와 짝수 길이 padding.
  - §3.1.2 Spacer block: 무시해도 되는 블록의 유일한 선례.
  - §3.1.5 Content blocks: segment와 parcel, 그리고 "모든 content block은 뒤따르는 metadata block이 기술해야 한다"는 규칙.
  - §3.2.1 Unencrypted Archive: footer, `metadata_backward_offset`, u16 `footer_size`.
  - §3.2.2 Encrypted Archive: age 기반 envelope. 안쪽에서는 spacer와 trailing byte를 금지한다.
  - §4.6 Partial access: 부분 결과와 손상 감지 의무의 범위.
  - §7 Block index: 05–0F가 예약되어 있다.

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
| **Par2** | 외부 파일. GF(2¹⁶) RS, slice 최대 32768개. slice마다 MD5와 CRC32. 패킷마다 magic, 길이, 자기 해시가 있고, main packet을 여러 번 중복 저장한다. | slice 해시로 손상 위치를 찾아 모든 오류를 **erasure로 바꾼다**. 패킷은 스캔만으로 찾을 수 있다. 단점: 인코딩이 O(N·M)이라 느리다. 행렬 구성이 항상 MDS는 아니라는 지적이 있다 *(확인 필요)*. |
| **Par3** (draft) | BLAKE3, FFT 기반 RS 등 여러 부호 중 선택. | 빠른 부호를 고를 수 있다. 다른 포맷 안에 recovery data를 넣는 경우도 다룬다고 기억한다 *(확인 필요)*. |
| **RAR recovery record와 .rev** | 아카이브 안에 크기의 n%를 RS로 덧붙인다. 볼륨 단위 복구용 .rev도 있다. | "파일 하나로 끝난다"는 편의가 사용자에게 실제로 먹힌다. |
| **FreeArc** | 0.67의 `-rr`. 이후 Bulat Ziganshin의 FastECC(GF(p) 위의 NTT 기반 O(n log n) RS). | 대용량에서는 O(n log n) 부호가 필수라는 문제의식. |
| **dvdisaster RS03** | 복구 데이터를 매체 전체에 interleave해 배치한다. | 국소 손상(긁힘, 연속 불량 섹터)과 복구 데이터가 같이 날아가지 않게 하는 배치. |
| 7z, zip, xz, zstd | 없음. | |

공통 교훈: **진짜 error correction은 필요 없다.** 저장 바이트 기준 slice 해시만 있으면 모든 손상이 위치를 아는 erasure가 되고, erasure 복구는 단순하고 강력하다.

## 2. 오류 모델 [잠정]

### 2.1 현실의 손상

- **HDD.** 섹터마다 내부 ECC가 있어서, 무작위 비트 오류는 대부분 "섹터 읽기 실패"(latent sector error, LSE)로 나타난다. 조용히 비트가 뒤집히는 경우는 드물다.
  - Bairavasundaram 등(SIGMETRICS 2007)과 Schroeder 등(FAST 2010)에 따르면 LSE는 독립적으로 생기지 않고, **LBA상으로도 시간상으로도 뭉친다**. 짧은 버스트가 대부분이지만 긴 꼬리가 있다.
  - Schroeder 2010은 디스크 내부 redundancy(interleaved parity 등)를 직접 평가했다. 이 설계와 가장 관련이 깊은 문헌이다 *(구체 수치 확인 필요)*.
- **조용한 손상.** Bairavasundaram 등(FAST 2008)과 Krioukov 등(FAST 2008)이 다룬 lost write, misdirected write, torn write, 펌웨어 버그가 원인이다. 단위는 비트가 아니라 **블록 하나가 통째로 틀린 내용**이고, 역시 공간적으로 뭉친다.
- **SSD.** Schroeder 등(FAST 2016)은 정정 불가 오류가 HDD보다 흔하다고 보고했다 *(확인 필요)*.
  - 순차로 쓴 아카이브는 같은 erase block에 들어갔을 가능성이 높다. 그래서 erase block 하나를 잃으면 **논리적으로 연속된 수 MiB가 통째로** 사라질 수 있다. 이 부분은 추론이다.
  - FTL 장애는 장치 전체 손실(E5)로 이어진다.
- **파일시스템과 전송.** 크래시 뒤 0으로 채워진 구간, extent 손실로 생긴 큰 구멍, 그리고 압도적으로 흔한 **truncation**(복사 중단, 용량 초과, 업로드 실패)이 있다. 네트워크 전송 자체에서는 TCP와 TLS 덕분에 비트 오류가 사실상 없다.
- **생성 시점의 RAM 비트 플립.** 손상된 데이터를 그대로 인코딩하므로 어떤 ECC로도 막지 못한다. 쓴 직후의 검증으로 대응한다.

### 2.2 오류 클래스

| 클래스 | 형태 | 빈도 | 목표 |
|---|---|---|---|
| **E1** 단일 단위 | 4 KiB 정렬 단위 하나의 erasure 또는 내용 오류 | 가장 흔함 | **항상** 견딘다 |
| **E2** 짧은 버스트 | 연속 수십 KiB ~ 수 MiB (LSE 클러스터, 긁힘, flash 블록) | 흔함 | 아카이브가 충분히 크면 **보장**한다 |
| **E3** 큰 영역 손실 | 수십 MiB 이상 (extent 손실, 큰 flash 장애) | 드묾 | best-effort. 전체 RS를 쓰면 정량적으로 보장한다 |
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
- **Spacer처럼 무시해도 되는 블록이라는 개념이 이미 있다.** recovery 관련 블록을 같은 지위로 두면, 복구 기능을 모르는 디코더도 아카이브를 그대로 읽는다. 05–0F 예약 구간도 비어 있다.
- **segment는 self-contained이고 parcel 구조가 있으며, 블록 간에 공유되는 압축 문맥이 없다**(dictionary 제외). 복구에 실패해도 손상 범위 밖의 파일은 살릴 수 있고, §4.6 Partial access가 이미 이를 허용한다.
- **segment를 찾는 경로가 둘이다.** 앞방향 스캔과 footer에서 시작하는 metadata 역방향 체인이다.
- **footer는 나머지에서 재구성할 수 있다.** `size_hint`는 길이에서, `metadata_backward_offset`은 스캔에서 얻는다. 그래서 보호 범위에서 뺄 수 있다. header magic은 상수이고, size hint block은 footer와 중복이다.

불리한 점:

- **블록 헤더에 sync marker가 없다.** 대부분 1바이트 leb128 type과 size뿐이라, size 필드 하나가 깨지면 앞방향 스캔이 탈선한다.
- **기존 checksum은 디코딩된 데이터에 대한 것이다.**
  - 저장 바이트의 손상 위치를 찾으려면 압축을 풀어 봐야 한다.
  - ~~dictionary, module, resource, formula block은 `checksum_method`가 None으로 고정되어 있어서, 손상을 감지할 방법이 아예 없다.~~ metadata, dictionary, module, resource block은 XXH64 checksum을 필수로 바꿨다(§5.2). formula block은 output checksum이 있을 때만 손상을 감지할 수 있다.
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

명세 §3.1.6 Mirror block에 반영했다. 원본 블록 타입 T에 대해 mirror 타입은 C00₁₆ + T이다(예: metadata 13₁₆ → C13₁₆). mirror의 size 필드는 일반 블록과 똑같이 자기 타입의 `ss`를 따르고 자기 data 길이를 센다. mirror가 원본보다 길어 size 필드가 한 단계 넓어져야 하면 타입이 한 칸 밀린 C01₁₆ + T를 쓴다(widened). 이 경우 `target_offset`에 1을 더해 표시한다. data는 다음과 같다.

| 이름 | 형 | 설명 |
|---|---|---|
| `mirror_magic` | u8[8] | 손상 시 스캔으로 찾기 위한 sync marker. 값은 미정(명세에 issue로 남김) |
| `target_offset` | leb128 ≠ 0 | 원본 블록의 `block_start`에서 **mirror block 자신의** `block_start`까지의 거리. widened이면 1을 더한다(거리는 항상 짝수). 원본은 항상 mirror 앞에 온다 |
| `target_data` | u8[*] | 원본 블록의 data를 그대로 담은 사본 |
| `mirror_checksum` | u64le | mirror의 `block_start`(type과 size 포함)부터 이 필드 직전까지의 XXH64 |

- 원본의 저장 바이트는 `leb128(T)`(최단 인코딩) ‖ size ‖ `target_data` ‖ padding과 정확히 같아야 한다. 따라서 원본과 mirror 모두 type을 최단 leb128로 써야 한다.
- repair는 원본 자리의 바이트가 재구성한 바이트와 다르면 덮어쓰기만 한다. 사본을 그 위치에서 블록으로 다시 해석하지는 않는다.
  > 다시 해석하지 않는 이유: metadata 안의 오프셋은 모두 metadata block 자신의 위치를 기준으로 한다. 다른 위치에서 해석하면 의미가 깨진다.
- **원본은 checksum이 있어야 한다.** checksum이 없으면 평소 읽기에서 원본의 손상을 알 수 없으므로, mirror로 넘어갈 계기가 없다. 그래서 metadata, dictionary, module, resource block은 checksum을 필수로 바꿨고(metadata는 10–17로 재할당, 18–1F는 예약), formula block은 output checksum이 있는 타입(74–77, 7C–7F)만 mirror 대상으로 삼는다.
  > metadata의 checksum 없는 모드는 처음 초안부터 있었고 근거가 남아 있지 않다. 성능 때문이었다면, 이 블록들은 아카이브의 작은 일부이고 한 번씩만 읽히므로 XXH64 비용은 무시할 수준이다. 반면 이름과 구조는 다른 어떤 checksum으로도 검증되지 않는다.
  > formula data 자체에 대한 checksum은 두지 않았다. 70–7F에 비트가 남아 있지 않고, 손상되면 output checksum이 잡는다. non-leaf 손상과의 구별은 non-leaf checksum이나 mirror로 다시 평가해 보는 것으로 한다. 부분 읽기에서는 감지할 수 없다.
- 처음 제안의 `target_hash`는 뺐다. 원본 checksum이 손상을 감지하고, `mirror_checksum`이 사본을 검증한다. verify는 사본과 원본을 바이트 비교할 수도 있다.
- 오프셋을 mirror 자신 기준으로 두면, archive_start나 체인이 깨져도 원본 위치를 알 수 있다.
- mirror 하나는 원본 하나만 담는다(타입이 원본마다 정해지므로). 한 원본에 mirror가 여럿 있어도 된다.
- mirror의 data는 원본보다 최소 17바이트 길다. 원본 size가 필드 한계에 가까울 때만 widened가 필요하고, 한 단계면 항상 충분하다. 필요할 때만 widened를 쓰는 것이 SHOULD이다.
  > 원본의 size 필드 폭은 `target_data` 길이에서 추론할 수 없다(필요 이상으로 넓은 size 필드가 허용된다). 그래서 widened 여부를 따로 표시해야 한다. 원본 쪽 size 필드를 넓히라고 요구하는 방식은 기존 아카이브에 나중에 mirror를 붙일 수 없게 만들어서 택하지 않았다.
- mirror는 어느 segment에도 속하지 않고 content block도 아니다. 마지막 metadata 뒤에도 올 수 있다. 해석 결과에는 영향이 없지만, 요건을 어긴 mirror가 있으면 invalid archive이다.
- encrypted archive 안에서는 일단 금지했다(§5.5, 명세에 issue).

보호 대상과 mirror 타입은 metadata(C10–C17), dictionary(C50–C57), module(C60–C63), resource(C64–C67), output checksum이 있는 formula(C74–C77, C7C–C7F) block이다.

#### 배치

길이 L인 버스트는, 원본과 사본의 거리 D가 L보다 짧을 때만 둘을 함께 지운다. 따라서:

- **E1:** 같은 4 KiB 단위만 공유하지 않으면 자동으로 해결된다.
- **E2:** "가능하면 D ≥ 4 MiB"를 SHOULD로 두는 것을 출발점으로 제안한다. 아카이브가 D보다 작으면 버스트 보장은 포기한다.
- **dictionary, module, formula:** 원래 segment 앞쪽에 있으므로, 사본을 segment 끝(metadata 근처)에 두면 자연히 멀어진다.
- **metadata:** 원본이 segment 끝에 있다. 그래서 segment k의 metadata 사본은 segment k+1의 데이터를 D 이상 쓴 뒤에 지연 배치한다. 마지막 segment의 사본은 footer 앞에 둘 수밖에 없어 거리를 확보할 수 없다. 이 손실은 §5.1의 segment 주기가 묶는다.
- **E3:** 거리를 고정값이 아니라 아카이브 크기의 비율로 잡으면 노려볼 수 있다. 다만 append 구조와 충돌하기 쉬우므로 best-effort로 둔다.

#### 찾기

마지막 metadata가 깨지면 체인의 출발점이 사라진다. 그때는 `mirror_magic` 스캔에 의존한다. repair 모드에서 아카이브 전체를 한 번 읽는 비용은 감수한다.

### 5.3 slice 해시 인덱스: 감지 전용 [제안, 선택]

- 저장 바이트를 `slice_size` 단위(2의 거듭제곱, 4 KiB 이상)로 나누고, slice마다 XXH64를 기록한다.
- parity 없이 인덱스만 두면 오버헤드가 0.01% 수준이고, 손상 위치를 정확히 짚을 수 있다.
- §5.2가 있으면 각 파일의 checksum으로 대부분 대체되므로 우선순위는 낮다. 다만 §5.4를 쓴다면 필수이다.

### 5.4 전체 RS parity (E3) [제안, 선택]

파일 내용까지 복구하려는 사용자를 위한 옵션이다. §5.2가 이미 있으므로, 이 단계의 복잡도는 순수하게 내용 보호의 비용이 된다.

- **블록 타입:** 예를 들어 08–0B를 recovery block으로 쓴다. 공통 앞부분은 `recovery_magic`, `self_hash`, `set_id`, `archive_offset`(archive_start로부터 이 블록까지의 거리), `kind`(index 또는 parity)이다.
  - `set_id`는 파라미터의 해시이다. 세대(generation)와 아카이브를 구분한다.
  - `archive_offset`이 있으면 header가 깨져도 slice 격자를 복원할 수 있다.
- **index 패킷:** `slice_size`, 보호 길이, set 배치, 부호 ID, slice 해시 목록(§5.3)을 담는다. 최소 두 벌을 서로 떨어진 위치에 둔다(Par2의 main packet 중복과 같은 발상).
- **보호 범위:** `[archive_start, archive_end)`에서 recovery block과 mirror block 영역을 0으로 간주한다. 이러면 자기 자신을 보호하는 순환이 없어진다. footer는 보호하지 않고 재구성한다.
- **부호:** GF(2¹⁶) 위의 additive-FFT 기반 RS(Lin–Chung–Han 계열, Leopard-RS가 대표 구현)를 제안한다.
  - MDS이므로 M개의 slice가 어떤 조합으로 손실돼도 복구된다.
  - O(n log n)이다.
  - 총 slice 수는 약 65536개로 제한된다 *(원본과 복구의 비율 제약은 확인 필요)*.
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

1. `mirror_magic`과 `recovery_magic`을 스캔하고, 각 블록의 `mirror_checksum`이나 `self_hash`를 검증한다.
2. mirror마다 원본을 사본과 비교하고, 다르면 사본으로 덮어쓴다. 비용이 싸고, 이후 RS 단계의 erasure를 줄여 준다.
3. recovery set이 있으면 slice 해시로 erasure를 표시하고 RS로 복원한다.
4. footer가 깨졌으면 앞방향 스캔으로 재구성한다.
5. 결과 바이트열을 `interpret an archive`에 넘긴다. 복원할 수 없는 부분은 §4.6 Partial access의 salvage로 넘긴다.

## 6. 범위 밖

- E5(장치나 파일 전체 손실): 별도 사본으로 대응한다.
- E6(shift): 필요해지면 rolling hash 기반 slice 재탐색을 따로 설계한다.
- 생성 시점의 RAM 비트 플립: 쓴 직후 검증으로 대응한다.
- 악의적 변조: 복구는 보안 기능이 아니다. 무결성과 인증은 암호화의 몫이다.

## 7. 열린 질문

1. ~~**mirror block 하나에 여러 원본을 담을지.**~~ 원본 하나만 담는다(§5.2).
2. ~~**`target_offset`의 부호.**~~ 원본은 항상 mirror 앞에 온다(§5.2).
3. **해시.** XXH64 하나로 통일할지, slice 인덱스에는 XXH3-128을 쓸지. 상대는 무작위 손상뿐이므로 64비트로 충분하다고 본다.
4. **"무시해도 되는 블록"의 명세 용어.** spacer, mirror, recovery를 묶는 이름이 필요하다. "auxiliary block"은 이미 dictionary, module, resource에 쓰고 있다. size hint block은 위치 제약이 있으므로 이 범주가 아니다.
5. **encrypted archive 안의 mirror 허용 여부.** 현재 안쪽에서는 spacer를 금지하며, mirror도 일단 금지했다.
6. **스크러빙.** 필요한 여유분은 "다음 검증 전까지 쌓일 손상"이 정한다. 그래서 `verify` 명령과 권장 주기를 도구 쪽에서 함께 설계해야 한다.

## 8. Action items

### 조사 (R)

- [ ] **R1.** Schroeder 등 FAST 2010의 LSE 버스트 길이 분포와 intra-disk redundancy 평가 결과를 확인하고, §2.2의 E2 범위와 §5.2의 D 기본값을 맞춘다.
- [ ] **R2.** Bairavasundaram 등 SIGMETRICS 2007·FAST 2008, Krioukov 등 FAST 2008, Schroeder 등 FAST 2016의 수치를 확인해 §2.1을 보강한다.
- [ ] **R3.** 현행 SSD의 erase block과 superblock 크기를 확인해 E2/E3의 경계를 검증한다.
- [ ] **R4.** Par2 행렬이 MDS가 아니라는 지적과, Par3 draft가 다른 포맷 안에 복구 데이터를 넣는 경우를 다루는지 확인한다.
- [ ] **R5.** Leopard 계열 구현(C++ leopard, Rust `reed-solomon-simd` 등)의 slice 수와 비율 제약, 라이선스, 처리 속도를 확인한다.

### 측정 (M)

- [ ] **M1.** 기존 말뭉치(`scripts/zstd-dict-*`의 cargo, rustdoc, pystdlib 등)에서 metadata, dictionary, module, resource, formula block이 아카이브 크기에서 차지하는 비율을 잰다. mirror 오버헤드의 근거가 된다.
- [ ] **M2.** segment 주기(예: 16, 64, 256 MiB)별 metadata 오버헤드와 디렉토리 반복 비용을 재서 §5.1의 기본값을 정한다.
- [ ] **M3.** (§5.4를 진행할 경우) FFT RS의 인코딩과 디코딩 처리량을 재서 압축 속도 대비 부담을 확인한다.

### 명세 (S)

- [ ] **S1.** "무시해도 되는 블록" 범주를 정의하고, spacer를 그 안으로 옮긴다(§7의 4번).
- [x] **S2.** mirror block 절을 추가한다. 구조와 배치 SHOULD를 포함한다. (`MIRROR_MAGIC` 값은 미정)
- [ ] **S3.** 해석과 분리된 "Repair" 절을 추가한다(§4의 1번, §5.6).
- [ ] **S4.** 비규범 노트를 추가한다. 내용은 오류 모델 요약, E5가 범위 밖이라는 점, segment 주기 권고, checksum 있는 블록 타입을 기본값으로 쓰라는 권고이다.
- [ ] **S5.** §7 Block index에 mirror 타입(반영함)과, §5.4를 진행하면 08–0B를 반영한다.
- [ ] **S6.** (§5.4를 진행할 경우) recovery block 절과 부호의 완전한 정의를 추가한다.
- [ ] **S7.** encrypted envelope의 trailer 확장안을 정한다(§5.5).

### 구현 (I)

- [ ] **I1.** E1–E4를 재현하는 fault injection 하네스를 만든다. 오류 모델을 그대로 시험 생성기로 옮기는 것이다. 4 KiB 정렬 단위 손상, 버스트, 0 채움, truncation을 다루고, 손상 위치는 무작위와 "중요 구조 조준" 두 방식으로 둔다.
- [ ] **I2.** compressor에 mirror block 생성과 segment 주기 옵션을 넣는다.
- [ ] **I3.** repair 도구를 만든다. 스캔, mirror 복원, footer 재구성, salvage 추출을 포함한다.
- [ ] **I4.** `verify` 명령을 만든다. checksum, mirror와 원본의 일치, slice 해시를 모두 검사한다.
- [ ] **I5.** I1로 "손상된 바이트가 속한 parcel만 잃는다"는 목표를 E1과 E2(D 이상 아카이브)에 대해 검증한다.
- [ ] **I6.** (선택) §5.4의 recovery set을 구현하고, I1로 L ≤ G·p·S 한계를 검증한다.

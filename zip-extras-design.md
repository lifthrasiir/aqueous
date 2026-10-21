# Aqueous `aqr-zip-extras` 설계 초안

ZIP 아카이브를 Aqueous로 변환할 때 ZIP extra field를 어떻게 다룰지 정한다. 즉 어떤 것을 Aqueous 모델로 흡수하고, 어떤 것을 금지하며, 어떤 것을 `aqr-zip-extras`에 그대로 남길지를 다룬다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다.

관련 코드:

- `spec/index.bs` §Optional metadata: `aqr-entry-extra`와 `aqr-zip-extras`의 CDDL, 그리고 이 문서가 해소하려는 TODO 주석.
- `spec/index.bs` §Entries: 엔트리 이름의 두 인코딩(WTF-8, `C1` 접두 raw 바이트열).
- `spec/index.bs` §Extraction: "extra는 무시해도 된다", "symlink와 hard link는 아직 모델에 없다".

출처 (모두 이번에 원문을 확인했다):

- PKWARE APPNOTE.TXT 6.3.10 (2022-11-01) §4.5 Extensible data fields, §4.6 Third Party Mappings.
- Info-ZIP `proginfo/extrafld.txt` (UnZip 6.0, 2008-07-17).
- `zipdetails` (Paul Marquess, `pmqs/zipdetails`)의 `%Extras` 표. 위 두 레지스트리에 없는 ID 대부분의 출처이다.
- minizip-ng `doc/mz_extrafield.md`, libarchive `archive_read_support_format_zip.c`.

---

## 0. 전제와 목표

- **[확정]** `aqr-zip-extras`는 엔트리별 `{ * id => bstr }` 맵이며, `aqr-entry-extra`의 키 -100에 들어간다.
- **[확정]** 용도는 **ZIP → Aqueous 단방향 변환**이다. 변환 결과에서 ZIP을 다시 만들어 낼 수 있어야 한다는 요구는 없다.
  > ZIP 파일 자체를 바이트 단위로 보존하는 recons(formula)는 별개의 문제이다. 그쪽은 ZIP을 바이트열로 다루므로 extra field의 의미를 신경 쓸 가능성이 낮다. 따라서 이 문서의 결정은 recons에 아무 제약도 주지 않는다.
- **[제안]** 위 목표에서 다음 원칙이 나온다.
  1. **정본은 하나만.** Aqueous 모델의 키(`mtime`, `unix-uid`, 엔트리 이름 등)로 옮길 수 있는 정보는 그 키에 둔다. `aqr-zip-extras`가 같은 정보의 정본이 되어서는 안 된다. 같은 정보가 두 군데 있으면 편집 도구가 한쪽만 고쳤을 때 둘이 어긋난다. 0x7075가 CRC를 들고 다니는 이유가 바로 이 문제이다.
  2. **ZIP 컨테이너에만 의미가 있는 것은 버린다.** 오프셋, 크기, 패딩, 정렬, 암호화 헤더, 저장된 바이트에 대한 서명이 여기에 해당한다. ZIP을 다시 쓰지 않으므로 이런 정보는 변환 순간 쓸모를 잃는다.
  3. **파일에 대한 정보인데 모델에 아직 자리가 없는 것은 불투명하게 보존한다.** OS별 속성, ACL, 리소스 포크 등이 여기에 해당한다. 나중에 모델이 커지면 전용 키로 승격한다. 그때 이미 변환해 둔 아카이브의 `aqr-zip-extras`에서 정보를 되살릴 수 있어야 한다는 것이 보존하는 주된 이유이다.
  4. **모르는 ID는 보존한다.** 금지 목록은 닫힌 목록이고, 나머지는 모두 허용한다. 모르는 필드가 원칙 2에 해당할 수도 있지만, 판단할 수 없으면 정보를 잃지 않는 쪽을 택한다.

## 1. 처분 분류

**[제안]** 각 header ID에 다음 처분 중 하나를 매긴다.

| 기호 | 이름 | `aqr-zip-extras`에서 | 변환 시 compressor의 동작 |
|---|---|---|---|
| **M** | 흡수 | MUST NOT | 내용을 모델 키로 옮기고 원본은 버린다. |
| **P** | 부분 흡수 | MAY | 옮길 수 있는 부분은 모델 키로 옮긴다. 옮길 수 없는 나머지가 있을 때만 원본 전체를 보존한다. |
| **X** | 폐기 | MUST NOT | 버린다. |
| **D** | 다이제스트 | MAY | 내용과 대조해 맞을 때만 보존한다. |
| **O** | 불투명 | MAY | 그대로 보존한다. |

**[제안]** 금지 위반에 대한 규칙은 다음과 같다.

- compressor는 M/X로 분류된 ID를 `aqr-zip-extras`에 쓰면 안 된다(MUST NOT).
- decompressor가 그런 값을 만나면 해당 항목만 무시해야 하며(SHOULD), 아카이브 전체를 거부하지는 않는다.
  > §Extraction이 이미 extra 전체를 무시해도 된다고 허용하므로, 일부를 무시하는 것은 자연스럽다. 아카이브 거부는 지나치다.

## 2. ID별 처분표

**[제안]** 출처 열의 A는 APPNOTE 6.3.10, I는 Info-ZIP `extrafld.txt`, Z는 `zipdetails`, 그 밖은 비고에 적었다.
처분이 M/P인 항목의 매핑 세부는 §3에 있다.

### 2.1 PKWARE 예약 범위 (0x0000–0x001F 및 PKWARE 지정)

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x0001 | Zip64 extended information | A I | **X** | 크기와 오프셋뿐이다. |
| 0x0007 | AV Info | A I | **X** | PKWARE Authenticity Verification. 원 바이트열에 대한 서명이다. |
| 0x0008 | Extended language encoding (PFS), 예약 | A I | O | 정의된 형식이 없다. 실제로 쓰인 사례를 모른다. |
| 0x0009 | OS/2 extended attributes | A I | O | local/central 형식이 다르다(§4.1). |
| 0x000a | NTFS (PKWARE Win95/WinNT) | A I | **M** | 이름과 달리 *시각*만 담는다. §3.1. |
| 0x000c | OpenVMS (PKWARE) | A I | O | 블록 안에 CRC가 있지만 블록 자신에 대한 것이므로 배치와 무관하다. |
| 0x000d | UNIX (PKWARE) | A I | **P** | atime, mtime, uid, gid는 흡수한다. link 대상과 device 번호는 남는다. §3.4. |
| 0x000e | File stream and fork descriptors, 예약 | A I | O | 정의된 형식이 없다. |
| 0x000f | Patch Descriptor | A I | O | 엔트리가 어떤 파일에 대한 패치인지 기술한다. 파일에 대한 정보이다. |
| 0x0014 | PKCS#7 Store for X.509 Certificates | A I | **X** | 서명 체계의 일부이다. |
| 0x0015 | X.509 Certificate ID and Signature (file) | A I | **X** | 서명. |
| 0x0016 | X.509 Certificate ID (central directory) | A I | **X** | central directory 바이트열에 대한 서명이다. |
| 0x0017 | Strong Encryption Header | A I | **X** | 암호화(§4.4). |
| 0x0018 | Record Management Controls | A I | O | 레코드 기반 파일 시스템용 속성이다. |
| 0x0019 | PKCS#7 Encryption Recipient Certificate List | A I | **X** | 암호화(§4.4). |
| 0x0020 | Timestamp record, 예약 | A | O | 정의된 형식이 없다. 서명 계열일 가능성이 높으나 확인할 수 없다. |
| 0x0021 | Policy Decryption Key Record | A | **X** | 암호화(Smartcrypt). |
| 0x0022 | Smartcrypt Key Provider Record | A | **X** | 암호화. |
| 0x0023 | Smartcrypt Policy Key Data Record | A | **X** | 암호화. |
| 0x0065 | IBM S/390 (Z390), AS/400 (I400) attributes | A I | O | |
| 0x0066 | 위의 압축형, 예약 | A I | O | |
| 0x4690 | POSZIP 4690, 예약 | A I | O | |

### 2.2 제3자 지정: 두 레지스트리 모두에 있는 것

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x07c8 | Info-ZIP Macintosh (old) | A I | O | |
| 0x2605 | ZipIt Macintosh (long) | A I | O | MacBinary로 저장된 파일이면 반드시 있다. 파일 내용을 해석하는 데 필요하므로 버리면 안 된다. |
| 0x2705 | ZipIt Macintosh 1.3.5+ (files) | A I | O | |
| 0x2805 | ZipIt Macintosh 1.3.5+ (directories) | A I | O | |
| 0x334d | Info-ZIP Macintosh (Mac3) | A I | O | local/central 형식이 다르다(§4.1). |
| 0x4154 | Tandem NSK | A I | O | |
| 0x4341 | Acorn/SparkFS | A I | O | |
| 0x4453 | Windows NT security descriptor | A I | O | local/central 형식이 다르다. 장차 전용 키 후보이다(§5). |
| 0x4704 | VM/CMS | A I | O | |
| 0x470f | MVS | A I | O | |
| 0x4854 | THEOS (old, unofficial) | A I | O | |
| 0x4b46 | FWKCS MD5 | A I | **D** | 비압축 내용의 MD5이다. |
| 0x4c41 | OS/2 ACL | A I | O | local/central 형식이 다르다. |
| 0x4d49 | Info-ZIP OpenVMS (obsolete) | A I | O | |
| 0x4d63 | Macintosh SmartZIP | A I | O | |
| 0x4f4c | Xceed original location | A I | O | |
| 0x5356 | AOS/VS ACL | A I | O | |
| 0x5455 | Extended timestamp ("UT") | A I | **M** | §3.1. |
| 0x554e | Xceed unicode ("NUCX") | A I | **M** | UTF-16LE 이름과 주석. §3.2. |
| 0x5855 | Info-ZIP UNIX type 1 ("UX") | A I | **M** | §3.1, §3.3. |
| 0x6375 | Info-ZIP Unicode Comment ("uc") | A I | **M** | §3.2. |
| 0x6542 | BeOS/Haiku | A I | O | local/central 형식이 다르다. |
| 0x6854 | THEOS | A I | O | |
| 0x7075 | Info-ZIP Unicode Path ("up") | A I | **M** | §3.2. |
| 0x7441 | AtheOS/Syllable | A I | O | local/central 형식이 다르다 *(확인 필요)*. |
| 0x756e | ASi UNIX | A I | **P** | mode, uid, gid는 흡수한다. symlink 대상과 SizDev는 남는다. §3.4. |
| 0x7855 | Info-ZIP UNIX type 2 ("Ux") | A I | **M** | §3.3. |
| 0x7875 | Info-ZIP UNIX type 3 ("ux") | A I | **M** | version 1만 흡수한다. 다른 version은 O. §3.3. |
| 0xa220 | Microsoft Open Packaging Growth Hint | A I | **X** | 제자리 편집용으로 확보해 둔 빈 공간일 뿐이다. |
| 0xfb4a | SMS/QDOS | I Z | O | APPNOTE는 0xfd4a로 적었다. Info-ZIP과 zipdetails는 0xfb4a이므로 APPNOTE의 오기로 보인다 *(확인 필요)*. 두 ID 모두 O로 둔다. |

### 2.3 APPNOTE에만 있는 것

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x1986 | Pixar USD header ID | A Z | **X** | USDZ의 64바이트 정렬용 패딩으로 알려져 있다 *(확인 필요)*. |
| 0x9901 | AE-x (WinZip AES) | A Z | **X** | 암호화(§4.4). 실제 압축 방식도 이 필드에 들어 있지만, 변환 시 이미 소비된다. |
| 0x9902 | unknown | A | O | |
| 0xa11e | Data Stream Alignment (Commons Compress) | A Z | **X** | ZIP 안에서 데이터 위치를 정렬하라는 요구와 패딩이다. ZIP을 다시 쓰지 않으므로 의미가 없다. |
| 0xcafe | Java JAR marker | A Z | **X** | 크기 0. 이 ZIP 파일 자체가 실행 가능한 JAR라는 표시이다. 파일이 아니라 컨테이너에 대한 정보이다. |
| 0xd935 | Android ZIP Alignment | A Z | **X** | 0xa11e와 같은 이유. |
| 0xe57a | ALZip (Korean) code page | A Z | **M** / O | 이름을 해당 코드 페이지로 디코딩해 WTF-8로 저장했으면 흡수된다(M). 디코딩하지 못해 이름을 raw(`C1` 접두)로 저장했을 때만 보존한다(O). 필드 형식 *(확인 필요)*. |

### 2.4 두 레지스트리 모두에 없는 것

이번 조사에서 `zipdetails`, minizip-ng, libarchive에서 찾은 것들이다.

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x10c5 | minizip CMS signature | minizip-ng | **X** | central directory에 대한 서명이다. |
| 0x1a51 | minizip Hash | minizip-ng | **D** | 비압축 내용의 MD5/SHA1/SHA256. 한 엔트리에 여러 개 올 수 있다(§4.2). |
| 0x4850 | Info-ZIP PHold ("PH") | Z | **X** | 나중에 다른 필드로 바꿔 쓰려고 잡아 둔 자리로 보인다 *(확인 필요)*. |
| 0x564b | Key-Value Pairs ("KV", SOZip) | Z | O | 서명 `"KeyValuePairs"` 뒤에 키-값 목록이 온다. 범용 메타데이터이다. |
| 0x5a4c | ZipArchive Unicode filename ("LZ") | Z | **M** | Artpol ZipArchive. 형식 *(확인 필요)*. 형식을 확인하기 전까지는 O로 취급한다. |
| 0x5a4d | ZipArchive offsets array ("MZ") | Z | **X** | 오프셋이다. |
| 0x6c78 | libarchive "xl" | libarchive | **M** | 스트리밍 추출을 위해 central 헤더의 version made by, 내부/외부 속성, 주석을 local 헤더에 복제한 것이다. 원본 central 헤더 필드를 매핑하는 과정에서 흡수된다. |
| 0x9903 | WinZip Reference | Z | **X** | 압축 방식 92(Reference, WinZip 25+)와 짝이다. 다른 엔트리를 가리키는 중복 제거 참조이다. 변환 시 참조를 풀고, 중복 제거는 Aqueous가 스스로 한다. |
| 0xcdcd | minizip Central Directory | minizip-ng | **X** | central directory 암호화/서명 체계의 일부이다. |

**[제안]** 위 표에 없는 ID는 모두 O이다. PKWARE 예약 범위(0–31)라도 등록되지 않은 ID이면 O이다.

## 3. 흡수 매핑

### 3.1 시각

**[제안]** 모델 키별 우선순위는 다음과 같다(왼쪽이 우선).
여러 출처가 같은 키를 주면 더 정밀한 쪽을 택한다. 정밀도가 같으면 더 최근 규격을 택한다.

| 모델 키 | 출처 (우선순위 순) |
|---|---|
| `mtime` | 0x000a Mtime → 0x5455 ModTime → 0x5855 ModTime ≈ 0x000d Mtime → DOS 날짜·시각 |
| `atime` | 0x000a Atime → 0x5455 AcTime (local만) → 0x5855 AcTime ≈ 0x000d Atime |
| `crtime` | 0x000a Ctime → 0x5455 CrTime (local만) |
| `ctime` | (출처 없음) |

인코딩:

- **0x000a**: `125(t − 116444736000000000)`, 즉 1601년 기준 100ns 단위를 1970년 기준으로 옮긴다. 값이 0이면 해당 시각이 없는 것으로 본다.
  > 0x000a의 `Ctime`은 Windows의 **생성 시각**이므로 `crtime`으로 가야 한다. POSIX `ctime`(상태 변경 시각)과 혼동하기 쉬우니 명세에 노트로 남겨야 한다.
- **0x5455, 0x5855, 0x000d**: UTC 초 단위이므로 untagged `int`로 쓴다.
  - **[잠정]** 부호 있는 32비트로 해석한다(Info-ZIP 문서 기준). 2038년 이후 값을 부호 없이 쓰는 구현이 있지만, 이 문서는 그 구분을 해결하지 않는다.
  - 0x5455의 central 버전은 ModTime만 갖고, flags는 *local*을 가리킨다. local 버전을 읽을 수 있으면 local을 쓴다.
- **DOS 날짜·시각**(local/central 헤더 필드): 시간대를 모르는 local time이므로 `[121(n)]`로 쓴다.

**[제안]** 시간대 오프셋을 복원한다. DOS 시각과 UTC 출처(0x000a, 0x5455 등)가 함께 있으면 둘의 차이로 원래 시간대를 추정할 수 있다. 차이 `d = dos_local − utc`(초)가 어떤 15분 배수 `900k`와 2초 미만으로 어긋나고 `k`가 `aqr-tz-offset`의 범위(−48..56) 안이면, `mtime`을 `[instant, k]`로 쓴다. instant는 UTC 출처의 정밀도를 따른다.
> 이렇게 하면 "언제"(UTC)와 "그 사람의 시계로 몇 시"(local)를 둘 다 남길 수 있다. 조건을 만족하지 않으면(시계 오차, DST 경계 등) DOS 시각은 버리고 UTC 출처만 쓴다.

**[제안]** 상충 처리: 정밀도가 다른 두 출처가 서로의 quantization interval을 벗어나도록 어긋나면, compressor는 경고하는 것이 좋다(SHOULD). 이 경우에도 우선순위가 높은 쪽을 택한다.

### 3.2 이름과 주석

**[제안]** 엔트리 이름의 출처 우선순위:

1. 0x7075 (version 1, NameCRC32가 헤더 이름과 일치). CRC가 다르면 APPNOTE대로 무시한다.
2. general purpose bit 11(EFS)이 켜진 헤더 이름. UTF-8로 해석한다.
3. 0x554e. UTF-16LE를 WTF-8로 옮긴다. lone surrogate도 무손실로 옮겨진다.
4. 0x5a4c *(형식 확인 필요)*.
5. 0xe57a가 지정한 코드 페이지로 헤더 이름을 디코딩한 결과.
6. 그 밖: compressor가 판단한 인코딩으로 디코딩한다. 판단할 수 없으면 raw(`C1` 접두)로 저장한다.

주석(`comment`)도 같은 방식이다. 순서는 0x6375 → EFS → 0x554e(central의 comment 부분) → 그 밖이다. 0x554e의 UTF-16 주석이 올바른 UTF-16이 아니면 `#6.273(bstr)`로 쓴다.

> 1–5번 경로에서는 원래의 비 UTF-8 이름 바이트(CP437 등)가 사라진다. 이 바이트는 ZIP 컨테이너의 인코딩 사정일 뿐이고 파일의 이름은 Unicode 쪽이므로, 원칙 2에 따라 버린다.

### 3.3 UID/GID

**[제안]** `unix-uid`와 `unix-gid`의 출처 우선순위는 0x7875 (version 1) → 0x7855 (local) → 0x756e ≈ 0x5855 (local, 있을 때) ≈ 0x000d이다.
Info-ZIP 문서가 정한 대로 0x7875가 0x7855보다 우선한다. 0x7855의 central 버전은 크기 0인 존재 표시일 뿐이다.

### 3.4 부분 흡수: 0x000d와 0x756e

**[제안]**
- 0x000d: atime, mtime, uid, gid를 흡수한다. 가변 부분이 비어 있으면 M과 같다. 가변 부분(hard/symbolic link 대상 또는 device major/minor)이 있으면 원본 전체를 보존한다.
- 0x756e: mode → `unix-mode`, uid, gid를 흡수한다. symlink 대상이 있거나 device 파일이면 원본 전체를 보존한다.
- 보존된 원본 안의 시각/uid/gid/mode는 정본이 아니다. 모델 키와 어긋나면 모델 키를 따른다(MUST).

> 이 상태는 과도기이다. 모델에 link와 device가 들어오면(§Extraction의 Issue) 둘 다 M이 된다. 그때까지는 정보를 잃지 않는 쪽을 택했다. 나머지만 잘라 저장하는 안도 있지만, 그러면 어느 레지스트리에도 없는 형식이 생기므로 원본을 그대로 두었다.

`unix-mode`의 출처 우선순위는 central 헤더의 external file attributes 상위 16비트(version made by의 host가 3 = Unix일 때) → 0x756e Mode이다.
`unix-mode`가 파일 형식 비트(`S_IFMT`)를 포함하는지는 현재 명세가 정하지 않았다. §6에서 정해야 한다.

## 4. 구조상의 문제

### 4.1 local과 central의 차이

ZIP의 extra field는 local 헤더와 central 헤더에 따로 있고, 둘의 내용이 다를 수 있다.
같은 ID라도 형식이 다른 경우가 많다. Info-ZIP의 압축형 속성 블록(0x0009, 0x4c41, 0x4453, 0x334d, 0x6542, 0x7441)은 central에 크기 요약만 둔다. 0x5455와 0x7855도 central 쪽이 줄어든 형식이다.

**[제안]** 맵에는 버전 하나만 보존한다. local 헤더에 해당 ID가 있으면 local 버전을, 없으면 central 버전을 쓴다.
> 알려진 사례에서는 local 쪽이 정보를 더 많이 담고, central 쪽은 local에서 유도할 수 있는 요약이다. 모르는 ID에서 두 버전이 다르면 central만의 정보를 잃을 수 있다. 이것을 막으려면 값 타입을 `bstr / [local: bstr / null, central: bstr / null]`로 넓혀야 하는데, 실사례를 모르는 상태에서 넓히는 것이 맞는지가 §6의 미해결 문제이다.

값 하나의 크기 상한은 65531바이트이다. extra field 전체 길이가 u16이고 각 필드 헤더가 4바이트이기 때문이다. 이 상한은 CDDL에서 `bstr .size (0..65531)`로 표현할 수 있다.

### 4.2 같은 ID의 중복

0x0015(X), 0x1a51(D)처럼 한 엔트리에 같은 ID가 여러 번 오는 경우가 있다. 맵 구조로는 이것을 표현할 수 없다.
X로 분류된 것은 상관없지만, D와 O에서는 문제가 된다.

**[제안]** 값을 `bstr / [2* bstr]`로 확장해 중복을 등장 순서대로 담는다. 현재로서는 0x1a51 말고 실사례를 모르므로, 이 확장 없이 "중복이면 첫 번째만 보존"으로 단순화하는 안도 있다.

### 4.3 잘못된 extra

예전 `zipalign`은 extra field를 0바이트로 채웠기 때문에, 4바이트 헤더 구조로 파싱되지 않는 꼬리가 생겼다. 길이 필드가 범위를 넘는 경우도 있다.
**[제안]** 이런 꼬리는 버린다. compressor는 경고하는 것이 좋다(SHOULD).

### 4.4 암호화된 ZIP

암호화된 엔트리는 복호화해서 옮길 수밖에 없다. 암호문을 그대로 두면 모델의 content가 평문이 아니게 되는데, 이는 Aqueous 모델과 맞지 않는다.
복호화하고 나면 0x0017, 0x0019, 0x0021–0x0023, 0x9901을 보존할 이유가 없다. Aqueous의 암호화는 age envelope로 따로 한다.

### 4.5 서명의 소실

X로 분류된 서명(0x0007, 0x0014–0x0016, 0x10c5)은 변환하면 사라진다. 변환된 Aqueous 아카이브에는 원래의 서명 대상(ZIP의 바이트열)이 남지 않으므로, 서명을 보존해도 검증할 수 없다.
서명된 JAR이나 APK는 META-INF나 APK Signing Block에 서명을 두므로 extra field와는 관계없다. JAR 서명은 엔트리 내용 기반이어서 META-INF 파일을 함께 옮기면 여전히 검증할 수 있다.

### 4.6 루트 디렉터리와 아카이브 전역

**[제안]** ZIP에는 루트 디렉터리 엔트리가 없으므로, 엔트리 0의 `zip-extras`는 쓰지 않는 것이 좋다(SHOULD NOT).
아카이브 전역 extra(첫 central 레코드에 실리는 0x0014 등)는 모두 X이므로, `aqr-opt-metadata`에 아카이브 단위의 `zip-extras`를 둘 필요는 없다.

## 5. 장차 전용 키로 승격할 후보

**[제안]** O로 분류한 것 가운데 다른 포맷(tar/pax, 7z, WIM 등)에도 대응물이 있어 모델 키로 승격할 가치가 있는 것들이다. 승격하면 해당 ID는 M이 된다.

| 후보 키 | ZIP 출처 | 다른 포맷의 대응물 |
|---|---|---|
| Windows 파일 속성 (readonly/hidden/system/archive 등) | external file attributes 하위 16비트 (extra가 아니다) | 7z, WIM, RAR |
| symlink 대상, hard link, device | 0x000d, 0x756e, 그리고 S_IFLNK 엔트리의 내용 | tar, pax, 7z |
| NT security descriptor | 0x4453 | WIM, `SCHILY.acl.*` 등 |
| 확장 속성(xattr)과 Mac 메타데이터 | 0x0009, 0x334d, 0x6542, 0x7441, 0x2605–0x2805 | pax `SCHILY.xattr.*`, AppleDouble |
| 내용 다이제스트 | 0x4b46, 0x1a51 | (Aqueous 자체 checksum과의 관계 정리 필요) |

> 현재 `spec/index.bs`의 TODO 주석 `NTFS attributes (ZIP extra 0x000a)`는 잘못되었다. 0x000a는 시각만 담는다. Windows 파일 속성은 extra가 아니라 central 헤더의 external file attributes에 있다.
> `UNIX attributes (ZIP extra 0x000d) - symlink target`도 정확하지 않다. Info-ZIP 계열은 symlink를 0x000d가 아니라 "S_IFLNK 모드 + 내용 = 대상 경로"로 저장하고, 0x000d를 쓰는 것은 PKWARE 계열뿐이다. symlink 대응은 extra field가 아니라 엔트리 모델 차원의 문제이다.

## 6. 미해결 문제

1. §4.1: 값 타입을 `[local, central]`로 넓힐 것인가, 아니면 "local 우선, 없으면 central"로 충분한가.
2. §4.2: 중복 표현을 채택할 것인가.
3. `unix-mode`가 `S_IFMT` 비트를 포함하는지.
4. 0x5455의 2038년 이후 값 해석(부호 있는 32비트 vs 부호 없는 32비트).
5. *(확인 필요)* 항목들, 즉 0x1986, 0x4850, 0x5a4c, 0xe57a의 형식과 0xfb4a/0xfd4a 중 어느 쪽이 맞는지 원 구현 소스로 확인해야 한다.
6. D 분류의 다이제스트가 맞지 않을 때, 버릴 것인가 아니면 손상 신호로 볼 것인가. 변환 원본이 손상되었을 가능성이 있다.

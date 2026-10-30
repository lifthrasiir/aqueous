# Aqueous `aqr-zip-extras` 설계 초안

ZIP 아카이브를 Aqueous로 변환할 때 ZIP extra field를 어떻게 다룰지 정한다. 즉 어떤 것을 Aqueous 모델로 흡수하고, 어떤 것을 금지하며, 어떤 것을 `aqr-zip-extras`에 그대로 남길지를 다룬다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다.

관련 코드:

- `spec/index.bs` §`zip-extras`: ZIP extra fields. 키의 의미, compressor/decompressor 규칙, 제거되는 extra field 목록(§Extra fields not to be preserved).
- `spec/index.bs` §Entries: 엔트리 이름의 두 인코딩(WTF-8, `C1` 접두 raw 바이트열). §Entry names from ZIP.
- `spec/index.bs` §Timestamps from ZIP.
- `spec/index.bs` §Archive model(link node, special node), §`link-group`: Hard links.
- `spec/index.bs` §Extraction: "extra는 무시해도 된다".
- `special-files-design.md` §6.6: ZIP 변환에서 link와 device의 처리.

출처 (모두 이번에 원문을 확인했다):

- PKWARE APPNOTE.TXT 6.3.10 (2022-11-01) §4.5 Extensible data fields, §4.6 Third Party Mappings.
- Info-ZIP `proginfo/extrafld.txt` (UnZip 6.0, 2008-07-17).
- `zipdetails` (Paul Marquess, `pmqs/zipdetails`)의 `%Extras` 표. 위 두 레지스트리에 없는 ID 대부분의 출처이다.
- minizip-ng `doc/mz_extrafield.md`, libarchive `archive_read_support_format_zip.c`.

---

## 0. 전제와 목표

- **[확정]** `aqr-zip-extras`의 구조와 규칙은 명세 §`zip-extras`에 반영했다.
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

금지 위반에 대한 규칙은 명세 §`zip-extras`에 반영했다. 명세는 M/X를 "Absorbed"/"Discarded"로 나열하고, P/D는 note로만 다루며, O는 기본값이라 따로 적지 않는다.

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
| 0x000d | UNIX (PKWARE) | A I | **M** | link 대상과 device 번호까지 모두 흡수한다. §3.4. |
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
| 0x7441 | AtheOS/Syllable | A I | O | BeOS 0x6542의 변형이다(숫자가 little-endian). local/central 형식이 다르다. central은 BSize와 Flags만 담은 5바이트이다(`extrafld.txt`). |
| 0x756e | ASi UNIX | A I | **P** | device가 아니면 모두 흡수한다. device이면 원본을 보존한다. §3.4. |
| 0x7855 | Info-ZIP UNIX type 2 ("Ux") | A I | **M** | §3.3. |
| 0x7875 | Info-ZIP UNIX type 3 ("ux") | A I | **M** | version 1만 흡수한다. 다른 version은 O. §3.3. |
| 0xa220 | Microsoft Open Packaging Growth Hint | A I | **X** | 제자리 편집용으로 확보해 둔 빈 공간일 뿐이다. |
| 0xfb4a | SMS/QDOS | I Z | O | APPNOTE는 0xfd4a로 적었으나 오기이다. Info-ZIP 구현(`unzpriv.h`와 `zip.h`의 `EF_QDOS 0xfb4a`, QDOS 포트가 바이트 `4A FB`를 쓰는 `qdos.c`)과 zipdetails는 모두 0xfb4a이다. 0xfd4a를 쓰는 구현은 찾지 못했다(libarchive, CPython, 7-Zip, minizip, libzip, commons-compress). 0xfd4a도 O로 둔다. 미등록 ID는 원래 O이기 때문이다. |

### 2.3 APPNOTE에만 있는 것

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x1986 | Pixar USD header ID | A Z | **X** | USDZ의 64바이트 정렬용 패딩이다. OpenUSD `pxr/usd/sdf/zipFile.cpp`가 "Arbitrarily chosen, unreserved ID"로 쓰고, data는 0으로 채운다. |
| 0x9901 | AE-x (WinZip AES) | A Z | **X** | 암호화(§4.4). 실제 압축 방식도 이 필드에 들어 있지만, 변환 시 이미 소비된다. |
| 0x9902 | unknown | A | O | |
| 0xa11e | Data Stream Alignment (Commons Compress) | A Z | **X** | ZIP 안에서 데이터 위치를 정렬하라는 요구와 패딩이다. ZIP을 다시 쓰지 않으므로 의미가 없다. |
| 0xcafe | Java JAR marker | A Z | **X** | 크기 0. 이 ZIP 파일 자체가 실행 가능한 JAR라는 표시이다. 파일이 아니라 컨테이너에 대한 정보이다. |
| 0xd935 | Android ZIP Alignment | A Z | **X** | 0xa11e와 같은 이유. |
| 0xe57a | ALZip (Korean) code page | A Z | **M** / O | data는 Windows 코드 페이지 번호(u32le, 예: 949, 932) 하나이다. 이름을 해당 코드 페이지로 디코딩해 WTF-8로 저장했으면 흡수된다(M). 디코딩하지 못해 이름을 raw(`C1` 접두)로 저장했을 때만 보존한다(O). 형식의 근거는 역공학 글 하나뿐이다. SharpZipLib issue #657이 같은 해석을 따른다. local과 central 중 어디에 오는지는 확인하지 못했다. |

### 2.4 두 레지스트리 모두에 없는 것

이번 조사에서 `zipdetails`, minizip-ng, libarchive에서 찾은 것들이다.

| ID | 이름 | 출처 | 처분 | 비고 |
|---|---|---|---|---|
| 0x10c5 | minizip CMS signature | minizip-ng | **X** | central directory에 대한 서명이다. |
| 0x1a51 | minizip Hash | minizip-ng | **D** | 비압축 내용의 MD5/SHA1/SHA256. 한 엔트리에 여러 개 올 수 있다(§4.2). |
| 0x4850 | Info-ZIP PHold ("PH") | Z | O | zipdetails 4.004(2024)가 이름만 넣었다. 공개된 Info-ZIP 소스(Zip 3.0, UnZip 6.0)와 APPNOTE에는 없다. 아마 미공개 Zip 3.1 계열에서 왔을 것이다. 이름으로 보면 자리 표시(X)일 것 같지만, 확인할 수 없으므로 원칙 4에 따라 O로 둔다. |
| 0x564b | Key-Value Pairs ("KV", SOZip) | Z | O | 서명 `"KeyValuePairs"` 뒤에 키-값 목록이 온다. 범용 메타데이터이다. |
| 0x5a4c | ZipArchive code page / filename ("ZL") | Z | **M** | Artpol ZipArchive의 `ZIP_EXTRA_ZARCH_NAME`이다. central에만 있다. 이름과 달리 Unicode가 아니라 코드 페이지 정보이다. §3.2. |
| 0x5a4d | ZipArchive offsets array ("MZ") | Z | **X** | ZipArchive의 `ZIP_EXTRA_ZARCH_SEEK`이다. 4.6.7 소스에는 정의만 있고 쓰는 곳이 없다. 이름으로 보아 오프셋이다. |
| 0x6c78 | libarchive "xl" | libarchive | **M** | 스트리밍 추출을 위해 central 헤더의 version made by, 내부/외부 속성, 주석을 local 헤더에 복제한 것이다. 원본 central 헤더 필드를 매핑하는 과정에서 흡수된다. |
| 0x9903 | WinZip Reference | Z | **X** | 압축 방식 92(Reference, WinZip 25+)와 짝이다. 다른 엔트리를 가리키는 중복 제거 참조이다. 변환 시 참조를 풀고, 중복 제거는 Aqueous가 스스로 한다. |
| 0xcdcd | minizip Central Directory | minizip-ng | **X** | central directory 암호화/서명 체계의 일부이다. |

**[제안]** 위 표에 없는 ID는 모두 O이다. PKWARE 예약 범위(0–31)라도 등록되지 않은 ID이면 O이다.

## 3. 흡수 매핑

### 3.1 시각

명세 §Timestamps from ZIP을 따른다.

### 3.2 이름과 주석

엔트리 이름은 명세 §Entry names from ZIP을 따른다.

주석은 명세 §Comments from ZIP을 따른다.

### 3.3 UID/GID

명세 §Unix metadata from ZIP을 따른다.

### 3.4 0x000d와 0x756e

**[제안]** 엔트리 종류의 판정과 Info-ZIP식 symlink는 `special-files-design.md` §6.6을 따른다. 두 필드는 다음과 같이 흡수한다.

- 0x000d: atime, mtime, uid, gid는 모델 키로 간다. 가변 부분은 symlink 대상이면 link node의 대상, hard link 대상이면 `link-group`, device major/minor(각각 u32le)이면 special node의 device 번호가 된다. 나머지가 없으므로 M이다.
- 0x756e: Mode의 권한 비트 → `unix-mode`, uid, gid는 모델 키로 간다. symlink 대상은 link node의 대상이 된다. external attributes에 모드가 없으면 Mode의 `S_IFMT`로 종류를 정한다.
- **[확정]** 0x756e의 device 번호(SizDev)는 `st_rdev` 원값이다(`extrafld.txt`). 비트 배치가 OS마다 다르고 version made by의 host 바이트는 Unix 계열을 구분하지 못하므로, (major, minor)로 나눌 수 없다. 이 경우에만 엔트리를 내용이 빈 file node로 두고 원본 0x756e를 보존한다. 그래서 0x756e는 P로 남는다.

`unix-mode`의 출처 우선순위는 명세 §Unix metadata from ZIP을 따른다.

## 4. 구조상의 문제

### 4.1 local과 central의 차이

ZIP의 extra field는 local 헤더와 central 헤더에 따로 있고, 둘의 내용이 다를 수 있다.
같은 ID라도 형식이 다른 경우가 많다. Info-ZIP의 압축형 속성 블록(0x0009, 0x4c41, 0x4453, 0x334d, 0x6542, 0x7441)은 central에 크기 요약만 둔다. 0x5455와 0x7855도 central 쪽이 줄어든 형식이다.

**[확정]** local 버전과 central 버전을 맵 키의 부호로 구분한다. 키의 의미, 생략 규칙, CDDL(`bstr .size (0..65531)` 포함)과 `−1−n`의 근거는 명세 §`zip-extras`에 반영했다.

- 처분(§1)은 키의 부호와 무관하게 ID `n`에 매긴다. P와 D는 두 버전을 따로 판단한다. 예를 들어 0x000d의 local 버전에만 link 대상이 있으면 양수 키만 남는다.

> 이 표현은 "central이 없다"와 "central이 local과 같다"를 구분하지 않는다. 변환이 단방향이므로(§0) 이 구분은 필요 없다. 같은 이유로 생략을 MUST NOT으로 해서 변환 결과가 결정적이 되도록 했다.
>
> 알려진 사례에서는 central이 local의 요약이므로 음수 키는 대개 쓸모가 없지만, 정보를 잃지 않는 쪽을 택한다(원칙 4). 흡수(M/P)할 때의 local/central 우선순위는 이와 별개로 §3이 정한다.

§4.2의 중복 표현을 채택하면, 같은 버전 안의 중복끼리 묶고 두 버전의 비교는 묶은 목록 전체로 한다.

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

엔트리 0의 `zip-extras`에 대한 규칙은 명세 §`zip-extras`에 반영했다.
아카이브 전역 extra(첫 central 레코드에 실리는 0x0014 등)는 모두 X이므로, `aqr-opt-metadata`에 아카이브 단위의 `zip-extras`를 둘 필요는 없다.

## 5. 장차 전용 키로 승격할 후보

**[제안]** O로 분류한 것 가운데 다른 포맷(tar/pax, 7z, WIM 등)에도 대응물이 있어 모델 키로 승격할 가치가 있는 것들이다. 승격하면 해당 ID는 M이 된다.

| 후보 키 | ZIP 출처 | 다른 포맷의 대응물 |
|---|---|---|
| Windows 파일 속성 (readonly/hidden/system/archive 등) | external file attributes 하위 16비트 (extra가 아니다) | 7z, WIM, RAR |
| NT security descriptor | 0x4453 | WIM, `SCHILY.acl.*` 등 |
| 확장 속성(xattr)과 Mac 메타데이터 | 0x0009, 0x334d, 0x6542, 0x7441, 0x2605–0x2805 | pax `SCHILY.xattr.*`, AppleDouble |
| 내용 다이제스트 | 0x4b46, 0x1a51 | (Aqueous 자체 checksum과의 관계 정리 필요) |

Windows 파일 속성은 `win-attrs`, 확장 속성과 리소스 포크는 `xattrs`와 `forks`로 명세에 들어갔다. ACL은 키 -8만
예약했다. 위 ID들을 M으로 옮기는 것은 ZIP 변환을 구현할 때 각 형식을 확인하고 한다.

## 6. 미해결 문제

1. ~~§4.1: 값 타입을 `[local, central]`로 넓힐 것인가, 아니면 "local 우선, 없으면 central"로 충분한가.~~ 키의 부호로 구분하기로 했다(§4.1).
2. §4.2: 중복 표현을 채택할 것인가.
3. 0x5455, 0x5855, 0x000d의 2038년 이후 값 해석(부호 있는 32비트 vs 부호 없는 32비트).
4. ~~*(확인 필요)* 항목들, 즉 0x1986, 0x4850, 0x5a4c, 0xe57a의 형식과 0xfb4a/0xfd4a 중 어느 쪽이 맞는지.~~ 0x4850만 남았다. 0xe57a는 근거가 역공학 하나이다. 0x1986, 0x5a4c, 0xfb4a는 원 구현 소스로 확인했다(§2).
5. D 분류의 다이제스트가 맞지 않을 때, 버릴 것인가 아니면 손상 신호로 볼 것인가. 변환 원본이 손상되었을 가능성이 있다.

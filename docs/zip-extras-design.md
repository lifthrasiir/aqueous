# Aqueous `aqr-zip-extras` 설계

ZIP을 Aqueous로 변환할 때 ZIP extra field를 어떻게 다룰지 정한 근거 문서: 어떤 것을 모델로 흡수하고, 어떤 것을 금지하며,
어떤 것을 `zip-extras`에 그대로 남길지. 구조와 규칙, 흡수(M)와 폐기(X)한 ID의 표는 `spec/index.bs`에 반영했다
(§`zip-extras`, §Extra fields not to be preserved, §Entry names from ZIP, §Timestamps from ZIP, §Comments from ZIP,
§Unix metadata from ZIP). 이 문서에는 명세에 없는 근거와 O/D 항목의 판단, 아직 안 한 것만 남겼다. 끝낸 일의 경과는 git
이력에 있다.

상태 표기는 `xform-design.md`와 같다. **[확정]**, **[잠정]**, **[제안]**.

관련: `special-files-design.md` §6.6(ZIP 변환에서 link와 device).

출처(원문 확인함): PKWARE APPNOTE.TXT 6.3.10 §4.5–4.6, Info-ZIP `proginfo/extrafld.txt`(UnZip 6.0), `zipdetails`의
`%Extras` 표(두 레지스트리에 없는 ID 대부분), minizip-ng `doc/mz_extrafield.md`, libarchive `archive_read_support_format_zip.c`.

---

## 0. 전제와 구현 상태

- **[확정]** 용도는 **ZIP → Aqueous 단방향 변환**이다. ZIP을 다시 만들 수 있어야 한다는 요구는 없다. ZIP을 바이트 단위로
  보존하는 recons(formula)는 별개이고 이 문서의 결정은 거기에 제약을 주지 않는다.
- 구현: aqr-unpack의 ZIP 읽기(`crates/aqr-unpack/src/zip.rs`)는 0x7075, 0x000a, 0x5455, 0x7855, 0x7875 등을 모델 키로
  흡수한다. `zip-extras` 자체는 아직 만들지 않는다(aqr-archive의 `Extra.zip_extras`는 있으나 zip.rs가 채우지 않는다).
  0x000d와 0x756e의 흡수(§3.4)도 아직 없다. 나머지 ID는 그냥 버려진다.
- 원칙(**[제안]**):
  1. **정본은 하나만.** 모델 키로 옮길 수 있는 정보는 그 키에 둔다. 같은 정보가 두 군데 있으면 편집 도구가 한쪽만 고쳤을
     때 어긋난다(0x7075가 CRC를 들고 다니는 이유).
  2. **ZIP 컨테이너에만 의미가 있는 것은 버린다**(오프셋, 크기, 패딩, 정렬, 암호화 헤더, 저장된 바이트에 대한 서명).
  3. **모델에 아직 자리가 없는 파일 정보는 불투명하게 보존한다**(OS별 속성, ACL, 리소스 포크). 나중에 전용 키가 생기면
     이미 변환한 아카이브에서 되살릴 수 있게 하는 것이 보존하는 이유다.
  4. **모르는 ID는 보존한다.** 금지 목록은 닫힌 목록이고, 판단할 수 없으면 정보를 잃지 않는 쪽을 택한다.

## 1. 처분 분류

| 기호 | 이름 | `zip-extras`에서 | 동작 |
|---|---|---|---|
| **M** | 흡수 | MUST NOT | 모델 키로 옮기고 원본은 버린다. |
| **P** | 부분 흡수 | MAY | 옮길 수 있는 부분은 옮기고, 나머지가 있을 때만 원본 전체를 보존한다. |
| **X** | 폐기 | MUST NOT | 버린다. |
| **D** | 다이제스트 | MAY | 내용과 대조해 맞을 때만 보존한다. |
| **O** | 불투명 | MAY | 그대로 보존한다(표에 없는 ID의 기본값, 등록되지 않은 PKWARE 예약 범위 0–31도). |

명세는 M/X를 "Absorbed"/"Discarded"로 나열하고 P/D는 note로만 다룬다.

## 2. ID별 처분

M과 X의 ID와 이유는 명세 §Extra fields not to be preserved를 본다. 아래는 명세에서 빠진 것: P, D, O 중 판단이 필요했던
것, 그리고 M/X 중 근거가 따로 필요한 것.

| ID | 이름 | 처분 | 비고 |
|---|---|---|---|
| 0x0008, 0x000e, 0x0020, 0x0066, 0x4690 | PFS 언어 인코딩, 파일 스트림 기술자, 타임스탬프 레코드, S/390 압축형, POSZIP | O | 정의된 형식이 없거나 예약뿐이다(0x0020은 서명 계열일 가능성이 높으나 확인할 수 없다). |
| 0x0009, 0x4c41, 0x4453, 0x334d, 0x6542, 0x7441 | OS/2 확장 속성과 ACL, NT security descriptor, Info-ZIP Mac3, BeOS/Haiku, AtheOS | O | local/central 형식이 다르다(§4.1). 0x7441은 0x6542의 변형(central은 BSize와 Flags의 5바이트). 0x4453은 장차 전용 키 후보(§5). |
| 0x000c, 0x000f, 0x0018, 0x0065, 0x4154, 0x4341, 0x4704, 0x470f, 0x4854, 0x6854, 0x4d49, 0x4d63, 0x4f4c, 0x5356, 0x07c8 | OpenVMS, Patch Descriptor, Record Management, AS/400, Tandem, Acorn, VM/CMS, MVS, THEOS, Info-ZIP VMS, SmartZIP, Xceed, AOS/VS, Info-ZIP Mac(old) | O | 파일에 대한 정보다. 0x000c는 블록 안의 CRC가 자신에 대한 것이므로 배치와 무관하다. |
| 0x2605, 0x2705, 0x2805 | ZipIt Macintosh | O | MacBinary로 저장된 파일이면 반드시 있고 내용을 해석하는 데 필요하므로 버리면 안 된다. |
| 0x4b46 | FWKCS MD5 | **D** | 비압축 내용의 MD5. |
| 0x1a51 | minizip Hash | **D** | 비압축 내용의 MD5/SHA1/SHA256, 한 엔트리에 여러 개 올 수 있다(§4.2). |
| 0x4850 | Info-ZIP PHold ("PH") | O | zipdetails 4.004가 이름만 넣었고 공개된 Info-ZIP 소스와 APPNOTE에는 없다(미공개 Zip 3.1 계열일 것). 이름으로는 자리 표시(X)일 듯하나 확인할 수 없어 원칙 4에 따라 O. |
| 0x564b | Key-Value Pairs ("KV", SOZip) | O | 서명 `"KeyValuePairs"` 뒤에 키-값 목록. 범용 메타데이터다. |
| 0x9902 | unknown | O | |
| 0xfb4a | SMS/QDOS | O | APPNOTE의 0xfd4a는 오기다. Info-ZIP 구현(`EF_QDOS 0xfb4a`)과 zipdetails가 0xfb4a다. 0xfd4a를 쓰는 구현은 찾지 못했다(libarchive, CPython, 7-Zip, minizip, libzip, commons-compress). 0xfd4a도 미등록이므로 O. |
| 0x756e | ASi UNIX | **P** | device가 아니면 모두 흡수. device이면 원본 보존(§3.4). |
| 0x7875 | Info-ZIP UNIX type 3 | **M** | version 1만 흡수, 다른 version은 O. |
| 0xe57a | ALZip (Korean) code page | **M** / O | data는 Windows 코드 페이지 번호(u32le) 하나. 이름을 그 코드 페이지로 디코딩해 WTF-8로 저장했으면 M, 디코딩하지 못해 raw(`C1`)로 저장했을 때만 보존(O). 형식의 근거는 역공학 글 하나뿐이고(SharpZipLib issue #657이 같은 해석), local과 central 중 어디에 오는지는 확인하지 못했다. |
| 0x5a4c | ZipArchive code page ("ZL") | **M** | central에만 있다. 이름과 달리 Unicode가 아니라 코드 페이지 정보(`ZIP_EXTRA_ZARCH_NAME`). |
| 0x6c78 | libarchive "xl" | **M** | central 헤더의 version made by, 속성, 주석을 local에 복제한 것이라 central 필드를 매핑하며 흡수된다. |
| 0x1986 | Pixar USD | **X** | USDZ의 64바이트 정렬 패딩(OpenUSD `zipFile.cpp`, data는 0). |
| 0x9901 | WinZip AES | **X** | 암호화. 실제 압축 방식도 여기에 있지만 변환 시 이미 소비된다. |
| 0x9903 | WinZip Reference | **X** | 압축 방식 92와 짝인 다른 엔트리 참조. 변환 시 참조를 풀고 중복 제거는 Aqueous가 한다. |
| 0xa11e, 0xd935, 0xcafe | 정렬 패딩(Commons Compress, Android), Java JAR marker | **X** | 컨테이너에 대한 정보다. |
| 0xa220 | Open Packaging Growth Hint | **X** | 제자리 편집용 빈 공간. |
| 0x5a4d | ZipArchive offsets ("MZ") | **X** | 4.6.7 소스에 정의만 있고 쓰는 곳이 없다. 이름으로 보아 오프셋. |

## 3. 흡수 매핑

시각, 이름, 주석, UID/GID는 명세(§Timestamps from ZIP, §Entry names from ZIP, §Comments from ZIP, §Unix metadata from ZIP)를
따른다. `unix-mode`의 출처 우선순위도 §Unix metadata from ZIP.

### 3.4 0x000d와 0x756e [제안, 미구현]

엔트리 종류의 판정과 Info-ZIP식 symlink는 `special-files-design.md` §6.6을 따른다.

- 0x000d: atime, mtime, uid, gid는 모델 키로. 가변 부분은 symlink 대상이면 link node의 대상, hard link 대상이면
  `link-group`, device major/minor(각각 u32le)이면 special node의 번호가 된다. 나머지가 없으므로 M.
- 0x756e: Mode의 권한 비트 → `unix-mode`, uid, gid는 모델 키로. symlink 대상은 link node의 대상. external attributes에
  모드가 없으면 Mode의 `S_IFMT`로 종류를 정한다.
- **[확정]** 0x756e의 device 번호(SizDev)는 `st_rdev` 원값이다(`extrafld.txt`). 비트 배치가 OS마다 다르고 version made
  by의 host 바이트는 Unix 계열을 구분하지 못하므로 (major, minor)로 나눌 수 없다. 이 경우에만 엔트리를 내용이 빈 file
  node로 두고 원본 0x756e를 보존한다. 그래서 P로 남는다.

## 4. 구조상의 문제

### 4.1 local과 central의 차이 [확정]

같은 ID라도 local과 central의 형식이 다른 경우가 많다(Info-ZIP의 압축형 속성 블록은 central에 크기 요약만 둔다. 0x5455와
0x7855도 central이 줄어든 형식). 맵 키의 부호로 구분한다(명세 §`zip-extras`). 처분은 키의 부호와 무관하게 ID에 매기고,
P와 D는 두 버전을 따로 판단한다(0x000d의 local에만 link 대상이 있으면 양수 키만 남는다). 이 표현은 "central이 없다"와
"central이 local과 같다"를 구분하지 않는데, 변환이 단방향이라 필요 없고 결정적이 되도록 생략을 MUST NOT으로 했다.
흡수할 때의 local/central 우선순위는 §3이 정한다.

### 4.2 같은 ID의 중복 [제안]

0x1a51처럼 한 엔트리에 같은 ID가 여러 번 오는 경우 맵으로는 표현할 수 없다(X는 상관없고 D와 O가 문제). 값을
`bstr / [2* bstr]`로 확장해 등장 순서대로 담는 안이 있다. 실사례가 0x1a51뿐이라 "중복이면 첫 번째만 보존"으로 단순화하는
안도 있다. 명세의 `; TODO: multiple extra fields with the same header ID`가 이것이다. 채택하면 같은 버전 안의 중복끼리
묶고 두 버전의 비교는 묶은 목록 전체로 한다.

### 4.3 잘못된 extra [제안]

예전 `zipalign`은 extra를 0바이트로 채워 4바이트 헤더 구조로 파싱되지 않는 꼬리를 만들었고, 길이 필드가 범위를 넘는
경우도 있다. 이런 꼬리는 버리고 compressor는 경고하는 것이 좋다(SHOULD).

### 4.4 암호화된 ZIP

암호화된 엔트리는 복호화해서 옮길 수밖에 없다(암호문을 두면 content가 평문이 아니게 되어 모델과 맞지 않는다). 그러면
0x0017, 0x0019, 0x0021–0x0023, 0x9901을 보존할 이유가 없다. Aqueous의 암호화는 age envelope로 따로 한다.

### 4.5 서명의 소실

X로 분류된 서명(0x0007, 0x0014–0x0016, 0x10c5)은 변환하면 사라진다. 서명 대상(ZIP의 바이트열)이 남지 않아 보존해도
검증할 수 없다. 서명된 JAR/APK의 서명은 META-INF나 APK Signing Block에 있어 extra와 관계없고, JAR 서명은 엔트리 내용
기반이므로 META-INF를 함께 옮기면 여전히 검증된다.

### 4.6 아카이브 전역

아카이브 전역 extra(첫 central 레코드의 0x0014 등)는 모두 X이므로 아카이브 단위의 `zip-extras`는 필요 없다.

## 5. 전용 키 승격 후보

O 가운데 다른 포맷에도 대응물이 있는 것. 승격하면 해당 ID는 M이 된다. Windows 파일 속성(`win-attrs`)과 확장 속성/리소스
포크(`xattrs`, `forks`)는 명세에 들어갔고, ACL은 키 -8만 예약했다. 아래 ID들을 M으로 옮기는 것은 ZIP 변환을 구현할 때 각
형식을 확인하고 한다.

| 후보 키 | ZIP 출처 | 다른 포맷의 대응물 |
|---|---|---|
| NT security descriptor | 0x4453 | WIM, `SCHILY.acl.*` |
| xattr와 Mac 메타데이터 | 0x0009, 0x334d, 0x6542, 0x7441, 0x2605–0x2805 | pax `SCHILY.xattr.*`, AppleDouble |
| 내용 다이제스트 | 0x4b46, 0x1a51 | (Aqueous 자체 checksum과의 관계 정리 필요) |

## 6. 미해결 문제

1. §4.2: 중복 표현을 채택할 것인가.
2. 0x5455, 0x5855, 0x000d의 2038년 이후 값 해석(부호 있는 32비트 vs 부호 없는 32비트).
3. D 분류의 다이제스트가 맞지 않을 때 버릴 것인가, 손상 신호로 볼 것인가(변환 원본이 손상되었을 수 있다).
4. 0x4850의 정체(§2). 0xe57a는 근거가 역공학 하나뿐이다.

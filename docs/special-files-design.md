# Aqueous 특수 파일(link, device) 설계

symbolic link, hard link, FIFO, device를 아카이브 모델에 어떻게 넣었는지에 대한 근거 문서. 설계는 2026-10-21에
`spec/index.bs`에 반영했고 구현도 끝났다(§9). 명세에 있는 내용은 뺐다. 남은 것은 명세에 없는 근거, 조사 결과,
기각한 안, 아직 안 한 것이다. 끝낸 일의 경과는 git 이력에 있다.

상태 표기는 `xform-design.md`와 같다. **[확정]** 합의됨, **[잠정]** 방향만 정함, **[제안]** 검토 전.

관련:

- `spec/index.bs` §Archive model(link node, special node, unknown node, link target), §Metadata block,
  §Optional metadata(`link-group`, `win-link-type`, `unix-mode`), §Extraction, §Merging segments, §Partial access.
- `zip-extras-design.md` §3.4, §5: 0x000d와 0x756e의 흡수.
- `xform-design.md` §4.1: "경로, 심링크, 파일별 체크섬, 같은 내용 파일의 중복 제거는 아카이브의 것이다."

---

## 1. 필요성 평가

| 종류 | 필요성 | 결정 |
|---|---|---|
| symbolic link | 높음. 없으면 흔한 트리가 깨진다. | 새 노드 종류(§4). |
| hard link | 중간. 저장 공간은 중복 제거가 풀고, 남는 것은 해제할 때의 inode 공유뿐이다. | 모델은 그대로 두고 extra의 `link-group`(§4.4). |
| FIFO | 낮음(시스템 이미지에서만). | **[확정]** symlink와 같은 틀에 얹는다. 해제는 명시적 요청이 있을 때만. |
| character/block device | 낮음(시스템 이미지에서만). root가 필요하고 번호는 OS 사이에 대응하지 않는다. | **[확정]** FIFO와 같다. (major, minor)로만 담고 출처 OS는 기록하지 않는다. |
| socket | 없음. 프로세스와 묶인 것이라 보관할 의미가 없다(tar도 건너뛴다). | 표현하지 않는다. |

### 1.1 symbolic link

없으면 깨지는 흔한 트리: 공유 라이브러리 버전 체인(복사하면 용량이 세 배), macOS `.framework`(링크를 복사로 바꾸면
코드 서명 검증이 실패한다: macOS 27에서 ad-hoc 서명한 프레임워크를 `cp -RL`로 복사해 확인했다. 최상위 `codesign -v`는
"bundle format is ambiguous", `Versions/A`는 "a sealed resource is missing or invalid"), git 작업 트리(mode 120000),
`node_modules/.bin`, 홈 백업, rootfs, 컨테이너 레이어.

모델에 symlink가 없으면 길은 셋이고 모두 나쁘다: 따라가기(의미가 바뀌고 순환과 트리 바깥의 내용이 딸려 온다), 버리기
(정보 손실; 이전의 aqr-git이 자체 symlink 목록을 둔 이유), Info-ZIP처럼 흉내 내기(§4.1에서 기각). 다른 주류 포맷이
모두 symlink를 지원하므로 변환에도 필요하다.

### 1.2 hard link

쓰이는 곳: rootfs(macOS 27의 `/usr/bin` 등에 링크 수 2 이상인 파일 385개, 개발 도구 shim 하나가 78개 이름; Debian
`perl-base`의 `perl`과 `perl5.x.y`; 직접 빌드한 git의 내장 명령, e2fsprogs의 `mkfs.ext*`. Debian과 CentOS의 git,
Debian의 e2fsprogs는 symlink로 바꾼다), `rsync --link-dest` 스냅샷 백업, pnpm 저장소.

크기는 중복 제거가 해결한다. hard link만이 주는 것은 해제 후의 inode 공유(스냅샷 복원의 디스크 사용량, 한쪽을 고치면
다른 쪽도 바뀐다는 의미)이다. 같은 내용이라는 이유만으로 hard link로 풀면 원래 독립이던 파일이 묶이므로, "원래 같은
inode였다"는 정보가 따로 필요하다.

### 1.3 FIFO와 device

나오는 곳은 사실상 시스템 이미지뿐이다(initramfs의 `/dev/console`, rootfs의 `/dev`, OCI 레이어의 overlayfs whiteout =
char device 0/0). 핵심 용례는 아니지만 tar/cpio/ZIP(0x000d)에서 불투명 blob 없이 손실 없이 옮기고 rootfs류를 왕복하려면
필요하고, symlink의 틀이 있으면 추가 비용이 바이트 몇 개와 해제 규칙 한 줄이다.

> 기각한 안: kind 번호만 예약하고 변환기가 device를 경고하며 버리거나 unknown node로 두게 하는 것. 비용이 너무 작고
> rootfs류를 왕복할 수 없다. 해제 기본값이 "만들지 않음"이므로 위험도 늘지 않는다.

## 2. 선행 사례

| 포맷 | symlink | hard link | device/FIFO |
|---|---|---|---|
| **tar** | typeflag `2`, `linkname` 또는 pax `linkpath` | typeflag `1`. 먼저 나온 *경로*를 가리킨다. | typeflag `3`/`4`/`6`. socket은 GNU tar가 건너뛴다. |
| **cpio newc** | `S_IFLNK` 모드, 내용이 대상 경로 | 같은 (dev, ino)의 엔트리들, 내용은 마지막 하나에만 | 모드와 `rdevmajor`/`rdevminor` |
| **ZIP (Info-ZIP)** | external attributes의 `S_IFLNK`, 내용이 대상 경로 | 없음 | 없음 |
| **ZIP (PKWARE)** | 0x000d 가변 부분 | 0x000d 가변 부분 | 0x000d(major/minor) |
| **RAR5** | redirection 레코드(extra 0x05): 타입 1 Unix symlink, 2 Windows symlink, 3 junction. flag 0x01은 "대상이 디렉터리". 데이터 없음. | 타입 4 hard link, 타입 5 file copy. 대상은 아카이브 안의 경로. | 없음 |
| **7z** | 전용 필드 없음. `-snl`이면 Unix는 attribute `0x8000`과 `st_mode << 16`(`S_IFLNK`)에 내용이 `readlink`, Windows는 `FILE_ATTRIBUTE_REPARSE_POINT`에 내용이 reparse data 버퍼. | 없음(`kpidHardLink`는 tar와 WIM만) | 없음 |
| **squashfs** | 별도 inode 형식 | inode 번호 공유 | 별도 inode 형식 |

배울 점: 내용으로 흉내 내는 방식(Info-ZIP, cpio, p7zip)은 모드를 무시하면 일반 파일로 떨어진다. "앞의 경로를 가리키는"
hard link(tar)는 순서에 의존한다. RAR5는 symlink와 junction을 구분하므로 Windows 왕복에는 이 구분이 필요하다.

## 3. 설계 원칙 [확정]

1. **모델을 바꾸는 것은 모델에, 해제 방식의 힌트는 extra에.** extra를 무시한 decompressor도 *다른* 트리를 만들면 안
   된다. symlink는 트리의 모양을 바꾸므로 모델에, hard link는 extra에 들어간다.
2. **모델 안의 경로 해석은 링크를 따라가지 않는다.** 링크 대상은 해석하지 않는 바이트열이다.
3. **정본은 하나만.** 파일 종류는 노드 종류가 정본이고 `unix-mode`는 종류를 담지 않는다(`zip-extras-design.md`의 원칙 1).
4. **위험한 것은 해제 기본값을 끈다.** device 생성은 `unix-uid`와 같은 취급이다.

## 4. 모델

### 4.1 기각한 안: 일반 파일 + `unix-mode`

Info-ZIP 방식(내용이 대상 경로, `unix-mode`의 `S_IFLNK`)은 채택하지 않는다. extra를 무시한 decompressor가 대상 경로가
담긴 일반 파일을 만들고(원칙 1 위반), 수십 바이트마다 leaf block이 필요하며, 이름만 나열하는 partial access가 content
block을 읽어야 하고, Windows에서 만든 아카이브가 Unix 모드를 지어내야 한다.

### 4.2 노드 종류

link node, special node, unknown node: 명세 §Archive model. symlink와 나머지를 나눈 이유는 symlink가 거의 모든
플랫폼에서 의미가 있고 실질적인 데이터(대상)를 갖는 반면, 나머지는 Unix 전용이고 데이터가 거의 없으며 해제 규칙이
다르기 때문이다(인코딩은 같은 틀을 공유한다). 이름 없는 링크는 해제할 곳이 없으므로 root는 link가 될 수 없다.

### 4.3 link target

명세 §Archive model. 대상이 `/`, `.`, `..`을 허용하고 비어 있으면 안 되는 이유는 대상이 경로이고 Linux는 빈 대상의
symlink를 만들 수 없기 때문이다(`ENOENT`). Windows 링크 내용의 `/`는 해석되지 않으므로(Go의 `os.Symlink`와 Git for
Windows가 `\`로 바꾼다) `\`→`/` 변환은 의미를 잃지 않는다. reparse data의 정본은 SubstituteName이고(PrintName은 표시용),
절대 링크와 junction의 `\??\`는 다른 플랫폼에서 의미가 없으므로 Rust `read_link`처럼 `\\?\`로 바꾼 뒤 가능하면 줄인다.
드라이브 문자 절대 경로(`C:/…`)는 플랫폼마다 의미가 달라 `\`→`/` 말고는 정규화하지 않는다.

### 4.4 hard link는 모델에 넣지 않는다

hard link로 묶인 파일은 모델에서 각각 독립된 `file node`이고, 같은 inode였다는 정보는 `link-group`(명세
§`link-group`: Hard links)이 담는다.

> 기각한 안: tar처럼 "다른 경로를 가리키는 노드". 트리가 DAG가 되어 segment 병합에서 대상이 바뀌거나 사라지는 경우를
> 모두 정의해야 하고, partial access가 대상 경로를 다시 찾아야 하며, 얻는 것이 없다(저장은 두 `file_param`이 같은 leaf
> block을 가리키면 되고 엔트리당 leb128 두 개). 지금 방식은 hard link를 모르는 decompressor도 올바른 결과를 만든다.

## 5. 인코딩

### 5.1 metadata block

명세 §Metadata block(`num_specials`, 특수 엔트리, `special_kind`). 대상을 엔트리 안에 인라인으로 둔 것은 `entry_offsets`로
바로 찾을 수 있어서이고, 특수 엔트리가 없는 아카이브의 비용은 헤더 1바이트다.

### 5.2 device 번호

(major, minor)를 따로 저장하고 출처 OS는 기록하지 않으며 해석은 해제하는 플랫폼에 맡긴다(명세 special node의 note).
근거:

- `dev_t` 하나로 표현하는 정본 형태가 없다. Linux 안에서도 인코딩이 여럿(old 8:8, 커널 12:20, `new_encode_dev` 32비트,
  glibc 64비트)이다. `mknod`는 어디서나 `makedev(major, minor)`를 받고, ustar, cpio newc, ZIP 0x000d도 쌍으로 저장한다
  (cpio odc/bin의 raw `c_rdev`는 이식성이 깨졌고 newc에서 쌍으로 바뀌었다).
- `dev_t`를 나누는 방식은 OS마다 다르다.

| OS | `dev_t` | major | minor |
|---|---|---|---|
| Linux(glibc) | 64비트 | 비트 8–19, 44–63 (32비트) | 비트 0–7, 20–43 (32비트) |
| Linux(커널) | 32비트 | 12비트 | 20비트 |
| macOS | 32비트 | 비트 24–31 | 비트 0–23 |
| FreeBSD 12+ | 64비트, `MMMmmmMm` | 32비트 | 32비트 |
| FreeBSD 11 이하 | 32비트 | 비트 8–15 | 나머지 24비트 |
| NetBSD | 32비트 | Linux 커널과 같다 | |
| OpenBSD | 32비트 | 비트 8–15 | 비트 0–7, 16–31 |

범위가 OS마다 다르므로 leb128로 저장한다.

- 번호의 의미도 OS마다 다르다(`/dev/null`: Linux c 1:3, macOS c 3:2; 첫 디스크: b 8:0 `sda`, b 1:0 `disk0`; macOS의
  `/dev/console`이 0:0이라 overlayfs whiteout과 번호가 같다). 의미가 있는 경우는 "Linux에서 나온 번호를 Linux에서
  푸는 것" 하나이고 tar도 같다. Linux는 정적 레지스트리가 있어도 동적 대역은 부팅마다 다르다. Windows에는 대응 개념이
  없다.
- 출처 OS를 기록하지 않는 이유: tar와 cpio에는 출처 정보가 없고 ZIP "version made by"의 host 바이트는 Unix 계열을
  구분하지 못하며, 채워져도 다른 OS가 할 수 있는 일은 거부뿐인데 그것은 해제 기본값(만들지 않고 보고)이 이미 한다.

### 5.3 `link-group`의 부호

명세 §`link-group`: Hard links. 처음에는 반대(0 이상이 전역)였다가 뒤집었다. `st_ino`는 `st_dev` 안에서만 유일하고
재사용되므로, 나중에 덧붙인 segment가 `st_ino`를 그대로 쓰면 앞 segment의 무관한 파일과 묶일 수 있고 내용까지 같으면
(빈 파일) 감지할 수 없다. 손쉬운 길이 위험한 쪽이 되므로, segment를 넘는 그룹은 compressor가 일관성을 책임지고 음수로
명시한다. 같은 그룹의 노드들이 `link-group`과 `comment`를 뺀 extra가 같아야 한다는 것을 MUST로 하지 않는 이유는 그룹의
일관성을 병합 때문에 최종 값에서만 판단할 수 있어서이다.

## 6. 기존 절과의 상호작용

### 6.1–6.2 segment 병합, partial access

명세 §Merging segments, §Archive model, §Partial access. 링크를 따라가는 조회를 만들 때 횟수 상한의 참고는 Linux의
`MAXSYMLINKS`(40).

### 6.3 compressor [확정]

- 기본적으로 symlink를 따라가지 않는다(SHOULD). 따라가는 옵션(`tar -h`)을 줄 때 순환 감지는 compressor의 몫이다.
- hard link는 `st_nlink > 1`이고 같은 `(st_dev, st_ino)`로 감지한다. 트리 바깥의 링크는 알 수 없어 그룹이 하나뿐인
  노드가 생길 수 있다.
- 덧붙이는 aqr-compress는 기존 트리의 번호를 피해 archive-wide(음수) 번호를 새로 매긴다(`crates/aqr-compress/src/archive.rs`,
  `Appending`). 기존 그룹을 모르는 compressor가 segment-local 번호를 써도 묶이지 않을 뿐 틀린 결과는 없다.

### 6.4 해제

규칙은 명세 §Extraction. 아래는 명세에 없는 조사 결과다.

고전적인 tar 공격(`a -> /etc` 다음에 `a/passwd`)은 모델에서 표현할 수 없다. 남는 경로는 대소문자 구분이 없는
파일 시스템에서 `A`(링크)와 `a/x`(파일)가 충돌하는 경우다(git CVE-2021-21300).

- GNU tar(1.35 `extract.c`)는 `-P` 없이 절대 경로나 `..`을 담은 symlink를 즉시 만들지 않고, 모드 0인 빈 파일을
  `O_EXCL`로 만들어 둔다. `..`을 담은 hard link와 자리 표시 파일을 가리키는 hard link도 같다. 해제가 끝나면 자리 표시
  파일의 (dev, ino, birthtime)이 그대로일 때만 링크로 바꾼다.
- libarchive의 `ARCHIVE_EXTRACT_SECURE_SYMLINKS`는 경로의 중간 성분이 디스크의 symlink이면 거부한다. bsdtar는 이것과
  `SECURE_NODOTDOT`를 기본으로 켠다. 이 플래그들은 바깥을 가리키는 symlink 자체는 거부하지 *않는다*(대상은 검사 없이
  `symlink()`에 넘긴다). GNU tar도 만들기를 미룰 뿐이다. unrar는 절대 경로나 안전하지 않은 상대 경로의 링크를 기본으로
  건너뛰고(`-ola`), 7-Zip은 25.01(CVE-2025-55188)부터 위험한 링크를 기본으로 거부한다(`-snld[N]`).

### 6.5 `unix-mode`와 `S_IFMT`

명세 §Optional metadata.

### 6.6 ZIP 변환 (`zip-extras-design.md`) [잠정]

구현된 것: aqr-unpack의 ZIP은 Info-ZIP식 symlink(external attributes의 `S_IFLNK`)를 link node로 읽는다
(`crates/aqr-unpack/src/zip.rs`; 내용이 비면 경고하고 버린다). 아직 없는 것:

- 0x000d와 0x756e의 흡수. 0x000d는 P에서 **M**으로 바뀐다: 가변 부분의 symlink 대상은 link node로, device 번호는
  special node로. 0x756e도 흡수하지만 device 번호가 `st_rdev` 원값이라 나눌 수 없으므로 device일 때만 P로 남는다
  (`zip-extras-design.md` §3.4).
- APPNOTE §4.5.7에 따르면 0x000d의 가변 부분은 link일 때 원 파일 이름(NUL 없음, 길이 TSize − 12), device일 때 major와
  minor(각각 u32le)다. hard link와 symlink를 구분하는 필드는 없으므로 external attributes의 모드로 구분한다. hard link
  이면 그 이름을 아카이브 안의 경로로 보고 그 엔트리와 같은 `link-group`을 준다(APPNOTE는 이것이 아카이브 경로라고
  명시하지 않는다). 해당 엔트리가 없으면 일반 파일로 두고 경고한다.
- `S_IFCHR`/`S_IFBLK`/`S_IFIFO` 모드의 엔트리를 special node로.

### 6.7 tar 등에서의 변환 [확정]

aqr-unpack(`tar.rs`, `rar.rs`, `sevenz.rs`)과 aqr-pack(`tar.rs`, `zip.rs`, `sevenz.rs`)이 한다. 규칙은 tar typeflag
`1`은 가리키는 엔트리와 같은 `link-group`, `2`와 pax `linkpath`는 link node, `3`/`4`/`6`은 special node. RAR5의 Windows
symlink와 junction은 `win-link-type`(대상이 디렉터리라는 flag는 symlink의 `file`/`directory`), file copy(타입 5)는
중복 제거로 흡수. 7z는 attribute `0x8000`과 `S_IFLNK`이면 내용이 대상인 link node, `FILE_ATTRIBUTE_REPARSE_POINT`이면
reparse data에서 symlink와 mount point 태그의 대상을 꺼내 link node로 만든다.

아직 없는 것: RAR의 hard link(타입 4)는 "왼 것"으로 알리고 풀지 않는다(`crates/aqr-unpack/src/rar.rs`의 `Kind::Left`).

### 6.8 그 밖

링크 대상은 metadata block에 인라인이므로 formula와 관계없고(`xform-design.md` §4.1의 경계와 맞는다), mirror block이 함께
보호한다. hard link 그룹이 leaf block 하나를 공유하면 그 블록의 손상이 그룹 전체에 미치는데, 중복 제거 일반과 같다.

## 7. 범위 밖

socket, 디렉터리 hard link, 그 밖의 Windows reparse point(볼륨 GUID를 가리키는 mount point, WSL symlink, AF_UNIX, 클라우드
placeholder, 데이터 중복 제거), macOS Finder alias(리소스 포크를 가진 일반 파일이라 xattr의 문제), OCI whiteout(tar 안의
`.wh.` 접두 일반 파일이라 모델과 무관; overlayfs의 char device 0/0은 special node로 표현된다), sparse file(저장 방식의
문제).

## 8. 링크 대상의 길이 상한

두지 않는다(명세 §Archive model의 note). 플랫폼별로 만들 수 있는 길이: Linux 4095바이트(ext4는 블록 크기 − 1, tmpfs는
페이지 크기 − 1, XFS는 1023), macOS APFS 1023(직접 시험), FreeBSD 1023, Windows는 reparse data 전체 16 KiB라 UTF-16로
약 8000자(구조체에서 계산한 값). 모든 플랫폼에서 만들 수 있는 것은 1023바이트까지이므로, 만들 수 없으면 해제 시
보고한다.

## 9. 구현 위치

- compressor의 수집(symlink, hard link의 `link-group`과 leaf block 공유, FIFO, device): `crates/aqr/src/meta.rs`, `main.rs`.
- 해제 규칙(§6.4): `crates/aqr/src/extract.rs`, `meta.rs`. 시험은 `crates/aqr/tests/names_unix.rs`의 `link_through_other_case`
  등(대소문자를 접는 파일 시스템에서 링크와 디렉터리의 충돌 포함).
- aqr-git의 자체 symlink 목록은 팩 모듈로 바뀌면서(2026-10-28) 없어졌고 link node가 대신한다.

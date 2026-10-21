# Aqueous 특수 파일(link, device) 설계 초안

Aqueous 아카이브 모델에 symbolic link, hard link, device 파일 같은 특수 파일을 넣을지, 넣는다면 어떻게 넣을지를 다룬다.
현재 명세는 §Extraction의 Issue("Symbolic links and hard links are not part of the archive model yet")로 이 문제를 미뤄 두었다.
본문은 현재 제안된 설계를 서술하고, 그렇게 정한 근거는 인용 블록에 둔다.
2026-10-21에 이 설계를 `spec/index.bs`에 반영했다(§9의 S 항목). FIFO/device는 처음에 보류했다가 같은 날 정의하기로 정하고 반영했다(§1.3, §5.2, S9). 반영하면서 달라진 점은 각 절에 적었다.
절과 항목의 상태 표기는 `xform-design.md`와 같다.

- **[확정]** 논의 전부터 정해져 있었거나 논의 중 명시적으로 합의된 것
- **[잠정]** 방향은 합의했으나 세부는 열려 있는 것
- **[제안]** 제안되었으나 아직 검토하지 않은 것
- **[보류]** 제안되었으나 결정을 미룬 것

출처를 다시 확인하지 않은 사실에는 *(확인 필요)*를 붙였다. 다른 포맷과 OS에 대한 서술은 2026-10-21에 명세와 구현 소스로 다시 확인했다(§9의 R 항목). 확인하지 못한 것은 본문에 그렇게 적었다.

관련 코드:

- `spec/index.bs` §Archive model: `archive value`, `file node`, `directory node`, `entry name`.
- `spec/index.bs` §Metadata block: 엔트리 번호 매기기(디렉터리 다음 파일), `entry_parents`, `file_params`, `entry`.
- `spec/index.bs` §Merging segments, §Partial access, §Extraction.
- `spec/index.bs` §Optional metadata: `unix-mode`와 TODO 주석 `UNIX attributes (ZIP extra 0x000d) - symlink target`.
- `zip-extras-design.md` §3.4, §5, §6의 3번: 0x000d와 0x756e의 부분 흡수, `unix-mode`의 `S_IFMT` 문제.
- `xform-design.md` §4.1: "경로, 심링크, 파일별 체크섬, 같은 내용 파일의 중복 제거는 아카이브의 것이다."
- `git-recons/src/container.rs`: 자체 컨테이너에 symlink 목록을 따로 두고 있다. 모듈 형태로 옮기면 이 목록은 아카이브가 맡아야 한다.

---

## 0. 전제

- **[확정]** 아카이브 모델은 값(value) 모델이다. `archive value`는 트리이고, 노드에는 식별자(identity)가 없다. 해석은 선언적으로 정의된다.
- **[확정]** 엔트리 이름은 `/`, `.`, `..`을 담을 수 없으므로 모델 안의 모든 경로는 단순한 성분의 나열이다. 이 성질은 지켜야 한다.
- **[확정]** 파일 시스템에 쓰는 것은 모델 밖의 일이고(§Extraction), extra는 무시해도 된다.
- **[확정]** 같은 내용의 중복 제거는 아카이브가 한다(`xform-design.md` §4.1). 두 파일이 같은 leaf block을 가리키는 것이 그 수단이다.
  > 명세가 이것을 명시적으로 허용하지는 않는다. 금지하지도 않는다. §5.3의 hard link 설계가 여기에 기대므로 명시해야 한다(S6).

## 1. 필요성 평가

**[제안]** 결론부터 말하면, 종류마다 필요성이 크게 다르다.

| 종류 | 필요성 | 이유 | 제안 |
|---|---|---|---|
| symbolic link | **높음** | 없으면 흔한 트리가 깨진다. 우회하면 정보가 사라지거나 의미가 바뀐다. | 모델에 새 노드 종류로 넣는다(§4). |
| hard link | 중간 | 저장 공간 문제는 중복 제거가 이미 푼다. 남는 것은 해제할 때의 inode 공유뿐이다. | 모델은 그대로 둔다. extra의 힌트로 표현한다(§5.3). |
| FIFO | 낮음 | 시스템 이미지에서만 나온다. OS 의존성은 없다. | **[확정]** symlink와 같은 틀에 싸게 얹는다. 해제는 명시적 요청이 있을 때만 한다. |
| character/block device | 낮음 | 시스템 이미지에서만 나온다. 해제하려면 root가 필요하고 보안 위험이 있다. 번호는 OS 사이에 대응하지 않는다(§5.2). | **[확정]** FIFO와 같다. 번호는 (major, minor)로만 담고 출처 OS는 기록하지 않는다. |
| socket | 없음 | 실행 중인 프로세스와 묶인 것이라 보관할 의미가 없다. tar도 건너뛴다. | 표현하지 않는다. |

### 1.1 symbolic link

symlink가 없으면 깨지는 흔한 트리:

- 공유 라이브러리 버전 체인(`libfoo.so -> libfoo.so.1 -> libfoo.so.1.2.3`). 해제한 결과가 동작은 해도 용량이 세 배가 된다.
- macOS `.framework` 번들의 `Versions/Current`와 최상위 링크. 링크를 복사로 바꾸면 코드 서명 검증이 실패한다.
  > macOS 27에서 ad-hoc 서명한 프레임워크를 `cp -RL`로 복사해 확인했다. 번들 최상위의 `codesign -v`는 "bundle format is ambiguous (could be app or framework)"로 실패한다. `Versions/A`만 검증하면 통과한다. 번들 안의 리소스 symlink는 서명이 봉인한다. 그것을 복사로 바꾸면 `Versions/A` 검증도 "a sealed resource is missing or invalid"로 실패한다.
- 소스 트리와 git 작업 트리(git의 mode 120000). `node_modules/.bin`, monorepo의 workspace 링크.
- 홈 디렉터리 백업, rootfs, 컨테이너 이미지 레이어.

모델에 symlink가 없으면 compressor가 고를 수 있는 길은 셋뿐이고, 모두 나쁘다.

1. **따라간다.** 링크 대상의 내용을 복사한다. 의미가 바뀌고, 순환이 생길 수 있으며, 아카이브 대상 트리 바깥의 내용이 딸려 들어온다.
2. **버린다.** 정보 손실이다. git-recons가 자체 컨테이너에 symlink 목록을 따로 둔 것도 이것을 피하려는 것이다.
3. **Info-ZIP처럼 흉내 낸다.** 내용을 대상 경로로 하고 `unix-mode`에 `S_IFLNK`를 넣는다. §4.1에서 기각한다.

다른 주류 포맷도 모두 symlink를 지원하므로 tar, ZIP, RAR, 7z에서 변환할 때 손실 없이 옮기려면 필요하다.

### 1.2 hard link

hard link가 쓰이는 곳:

- rootfs. 같은 바이너리가 여러 이름으로 링크되어 있는 경우가 흔하다.
  - macOS 27의 시스템 볼륨에는 `/usr/bin`, `/usr/sbin`, `/usr/libexec` 아래에만 링크 수가 2 이상인 파일이 385개 있다. 개발 도구 shim 하나가 78개 이름을 갖고, `at`/`atq`/`atrm`도 한 파일이다.
  - Debian `perl-base`의 `/usr/bin/perl`과 `perl5.x.y`는 hard link이다(`debian/rules`의 `ln`).
  - upstream의 기본 설치가 hard link인 경우도 있다. git `make install`은 내장 명령(`git-add` 등)을, e2fsprogs는 `mkfs.ext*`와 `fsck.ext*`를 hard link로 만든다. 다만 Debian과 CentOS Stream의 git, Debian의 e2fsprogs는 이를 symlink로 바꾼다. 따라서 이 경우는 직접 빌드해 설치한 트리에서 나온다.
  - coreutils를 hard link로 설치하는 배포판은 찾지 못했다. single-binary 빌드는 symlink나 shebang 스크립트를 쓴다.
- `rsync --link-dest`나 그와 비슷한 방식으로 만든 스냅샷 백업. 수십 개의 스냅샷이 대부분의 파일을 공유한다.
- 패키지 관리자의 저장소(pnpm의 content-addressable store 등).

이 중 아카이브 *크기*의 문제는 중복 제거가 이미 해결한다. hard link만이 주는 것은 해제 후에도 같은 inode를 공유한다는 점이다. 이것은 두 가지 이유로 중요하다.

- 해제한 결과의 디스크 사용량. 스냅샷 백업을 복원할 때는 이것이 결정적이다.
- 의미. 한쪽을 고치면 다른 쪽도 바뀐다는 성질에 기대는 도구가 있다.

반면 같은 내용이라는 이유만으로 hard link로 해제하면 안 된다. 원래 독립이던 파일이 묶여 버린다. 따라서 중복 제거와 별도로 "이 파일들은 원래 같은 inode였다"는 정보가 필요하다.

### 1.3 FIFO와 device [확정]

**[확정]** FIFO, character device, block device를 special node로 정의한다(§4.2, §5.1). 해제 기본값은 "만들지 않음"이다(§6.4).

나오는 곳은 사실상 시스템 이미지뿐이다. initramfs의 `/dev/console`, chroot와 rootfs의 `/dev`, OCI 레이어의 overlayfs whiteout(char device 0/0)이 그 예이다.
Aqueous의 주 용례(배포, 백업, 재압축)에서 이것들이 핵심은 아니다. 그래도 넣는 이유는 다음과 같다.

- tar, cpio, ZIP(0x000d)에서 변환할 때 불투명 blob(`zip-extras`) 없이 손실 없이 옮길 수 있다. rootfs tarball, initramfs, 컨테이너 레이어를 재압축하는 것은 범위 안이고, 빼면 이것들은 왕복하지 못한다.
- symlink를 위한 틀(§4.2)이 생기면 추가 비용이 거의 없다. 바이트 몇 개의 payload와 해제 규칙 한 줄이면 된다.

FIFO와 device는 사정이 다르다.

- FIFO는 payload가 없고 OS 의존성도 없다(POSIX `mkfifo`). 망설일 이유가 거의 없다.
- device는 번호의 의미가 OS에 묶여 있다(§5.2). 그래서 넣을지는 이식성이 아니라 tar/cpio를 손실 없이 대신할 수 있느냐로 판단했다.

> 넣지 않는 안도 있었다. kind 번호만 예약해 두고, 변환기가 device를 만나면 경고하고 버리거나 unknown node로 두게 하는 것이다. 비용이 너무 작고, 그러면 rootfs류를 왕복할 수 없어서 기각했다. 해제 기본값을 "만들지 않음"으로 두므로 위험도 늘지 않는다(§6.4).

## 2. 선행 사례

| 포맷 | symlink | hard link | device/FIFO |
|---|---|---|---|
| **tar (ustar/pax)** | typeflag `2`, `linkname`(100바이트) 또는 pax `linkpath` | typeflag `1`. 먼저 나온 *경로*를 가리킨다. | typeflag `3`/`4`(devmajor/devminor), `6`. socket은 GNU tar가 건너뛴다. |
| **cpio (newc)** | `S_IFLNK` 모드, 내용이 대상 경로 | 같은 (dev, ino)를 가진 엔트리들. 내용은 마지막 하나에만 있다. | `S_IFCHR` 등의 모드와 `rdevmajor`/`rdevminor` |
| **ZIP (Info-ZIP)** | external attributes의 `S_IFLNK`, 내용이 대상 경로 | 없음 | 없음 |
| **ZIP (PKWARE)** | 0x000d 가변 부분 | 0x000d 가변 부분 | 0x000d 가변 부분(major/minor) |
| **RAR5** | file system redirection 레코드(extra 0x05): 타입 1 Unix symlink, 2 Windows symlink, 3 junction. flag 0x01은 "대상이 디렉터리"이다. 대상은 NUL 없는 UTF-8이다. 데이터는 저장하지 않는다. | 같은 레코드의 타입 4(hard link). 타입 5(file copy)도 있다. 둘 다 대상은 아카이브 안의 다른 엔트리 경로이다(unrar는 해제 디렉터리 기준으로 풀어 `link`하거나 복사한다). | 없음. 레코드가 없고, unrar도 만들지 않는다. |
| **7z** | 전용 필드가 없다. `-snl`을 주면 Unix에서는 attribute에 `0x8000`(`FILE_ATTRIBUTE_UNIX_EXTENSION`)과 `st_mode << 16`(`S_IFLNK`)을 넣고 내용을 `readlink` 결과로 쓴다. Windows에서는 `FILE_ATTRIBUTE_REPARSE_POINT`를 세우고 내용을 reparse data 버퍼 그대로 쓴다. `-snl`이 없으면 링크를 따라간다. | 없음. `-snh`의 `kpidHardLink`는 tar와 WIM 핸들러만 쓰고, 7z에는 일반 사본으로 들어간다. | 없음. attribute의 모드 비트에 `S_IFCHR` 등이 남을 뿐이다. |
| **squashfs** | 별도 inode 형식, 대상은 inode 안에 있다. | inode 번호 공유 | 별도 inode 형식 |

배울 점:

- **내용으로 흉내 내는 방식(Info-ZIP, cpio, p7zip)은 모두 "모드를 무시하면 일반 파일"로 떨어진다.** Windows에서 unzip한 결과에 대상 경로만 담긴 텍스트 파일이 생기는 것이 그 결과이다.
- **"앞의 경로를 가리키는" hard link(tar)는 순서에 의존한다.** 앞의 엔트리를 빼거나 바꾸면 깨진다. 대상이 디렉터리이거나 존재하지 않는 경우도 처리해야 한다.
- **RAR5는 symlink와 junction을 구분한다.** Windows에서 손실 없이 왕복하려면 이 구분이 필요하다.

## 3. 설계 원칙 [확정]

1. **모델을 바꾸는 것은 모델에, 해제 방식에 대한 힌트는 extra에 둔다.** extra를 무시한 decompressor도 *다른* 트리를 만들어서는 안 된다. symlink는 트리의 모양을 바꾸므로 모델에 들어간다. hard link는 트리의 모양을 바꾸지 않으므로 extra에 들어간다.
2. **모델 안의 경로 해석은 링크를 따라가지 않는다.** 모든 경로는 계속 단순한 성분의 나열이다. 링크 대상은 해석하지 않는 바이트열이다.
3. **정본은 하나만.** 파일 종류는 노드 종류가 정본이다. `unix-mode`는 종류를 담지 않는다(§6.5). `zip-extras-design.md`의 원칙 1과 같다.
4. **위험한 것은 해제 기본값을 끈다.** device 생성은 `unix-uid`와 같은 취급을 받는다.

## 4. 모델

### 4.1 기각한 안: 일반 파일 + `unix-mode`

**[제안]** Info-ZIP 방식, 즉 `file node`의 내용을 대상 경로로 두고 `unix-mode`의 `S_IFLNK`로 표시하는 방식은 채택하지 않는다.

> - extra를 무시할 수 있다는 규칙(§Extraction) 때문에, 무시한 decompressor는 대상 경로가 담긴 일반 파일을 만든다. 원칙 1에 어긋난다.
> - 파일마다 leaf block이 하나씩 필요하다. 수십 바이트짜리 대상 경로마다 블록 헤더와 parcel이 생긴다.
> - 이름만 나열하는 partial access(`ls -l`에 해당)가 content block을 읽어야 한다.
> - `unix-mode`가 Unix 전용 키이므로 Windows에서 만든 아카이브가 symlink를 표현하려면 Unix 모드를 지어내야 한다.

### 4.2 새 노드 종류 [확정]

**[확정]** `archive value`에 link node와 special node를 추가한다. 모르는 `special_kind`(4 이상)를 담는 unknown node(**kind**, **data**, **extra**)도 추가했다(§5.1).

A **link node** is a struct with the following items:

- **target**, a *link target*.
- **extra**, a CBOR data item matching `aqr-entry-extra`, or null.

A **special node** is a struct with the following items:

- **kind**, one of "`fifo`", "`char-device`" or "`block-device`".
- **device**, a pair of non-negative integers (major, minor) if **kind** is a device, or null otherwise.
- **extra**, a CBOR data item matching `aqr-entry-extra`, or null.

`directory node`의 **children** 값에 link node와 special node를 허용한다. `archive value`의 **root**는 계속 directory node나 file node만 될 수 있다.

> symlink와 나머지를 다른 노드로 나눈 이유: symlink는 거의 모든 플랫폼에서 의미가 있고 대상이라는 실질적인 데이터를 갖는다. 나머지는 Unix 전용이고 데이터가 거의 없다. 해제 규칙(§6.4)도 다르다. 인코딩(§5.1)에서는 같은 틀을 공유한다.
>
> root를 link로 두는 것은 의미가 없다. 이름 없는 링크는 해제할 곳이 없다.

### 4.3 link target [확정]

A **link target** is a struct with the following items:

- **encoding**, either "`wtf-8`" or "`raw`".
- **bytes**, a non-empty byte sequence not containing 00₁₆, which is a valid WTF-8 string if **encoding** is "`wtf-8`".

- `entry name`과 달리 `/`, `.`, `..`을 허용한다. 대상은 경로이고, 절대 경로일 수 있으며, 존재하지 않는 곳(dangling)을 가리킬 수 있다.
- 구분자는 `/`이다. Windows에서 만든 compressor는 `\`를 `/`로 바꿔야 한다(MUST).
  > Windows의 파일 이름에는 `/`가 올 수 없다. 또 링크 내용의 `/`는 해석되지 않는다(Go의 `os.Symlink`가 "'/' does not work in link's content"라며 `\`로 바꾸고, Git for Windows도 그렇게 한다). 그래서 이 변환은 의미를 잃지 않는다. 해제할 때는 `/`를 `\`로 되돌린다.
  >
  > reparse data에는 SubstituteName과 PrintName이 있다. 정본은 SubstituteName이다. PrintName은 표시용이고 생략될 수 있다. 상대 링크(`SYMLINK_FLAG_RELATIVE`)는 접두 없이 그대로 저장된다. 절대 링크와 junction은 NT 경로이므로 `\??\`로 시작한다. compressor는 이것을 Go의 `os.Readlink`처럼 Win32 경로로 바꾼 뒤 구분자를 바꾸는 것이 좋다(SHOULD).
  > - `\??\C:\foo` → `C:/foo`
  > - `\??\UNC\server\share\x` → `//server/share/x`
  > - `\??\Volume{GUID}\x` → `//?/Volume{GUID}/x`
  >
  > Rust의 `read_link`는 `\??\`를 `\\?\`로 바꾼 뒤 가능하면 일반 경로로 줄인다. 결과는 같은 방향이다. `\??\`가 그대로 새어 나오면 다른 플랫폼에서 의미가 없다.
- 비어 있으면 안 된다. Linux에서는 빈 대상의 symlink를 만들 수 없다(`ENOENT`).
- 길이 상한은 두지 않는다. 구현이 상한을 두는 것은 §Conformance의 일반 규칙을 따른다.
- **[확정]** 드라이브 문자 절대 경로(`C:/…`) 등은 `\`를 `/`로 바꾸는 것 말고는 정규화하지 않고 플랫폼에 맡긴다. `\??\` 변환은 명세에서 note로만 권한다.

**[잠정]** Windows의 symlink에는 "파일 symlink"와 "디렉터리 symlink"의 구분이 있고, 그와 별도로 junction이 있다. 이것은 해제 방식에 대한 힌트이므로 extra에 둔다(§5.3의 `win-link-type`).
> extra가 없을 때 Windows의 decompressor는 대상을 아카이브 안에서 해석해 디렉터리이면 디렉터리 symlink를 만들 수 있다(§6.4).

### 4.4 hard link는 모델에 넣지 않는다 [확정]

**[확정]** hard link로 묶인 파일들은 모델에서 각각 독립된 `file node`이다. 각 노드는 자기 내용을 가진다. 같은 inode였다는 정보는 extra의 `link-group`(§5.3)이 담는다.

> 대안은 tar처럼 "다른 경로를 가리키는 노드"를 두는 것이다. 다음 이유로 기각한다.
>
> - 값 모델에 식별자가 생긴다. 노드가 다른 노드를 경로로 가리키면 트리가 DAG가 되고, segment 병합(§6.1)에서 대상이 바뀌거나 사라지는 경우를 모두 정의해야 한다.
> - 순서 의존성이 생긴다. partial access로 한 경로를 읽으려 해도 대상 경로를 다시 찾아야 한다.
> - 얻는 것이 없다. 저장 공간은 두 `file_param`이 같은 leaf block을 가리키게 하면 그대로 아낄 수 있다. 엔트리당 비용은 leb128 두 개 정도이다.
>
> 이 방식이면 hard link를 모르는 decompressor도 올바른 결과(같은 내용의 독립된 파일들)를 만든다. 원칙 1을 만족한다.

## 5. 인코딩

### 5.1 metadata block [확정]

**[확정]** 엔트리에 세 번째 부류를 추가한다.

- 헤더에 `num_specials`(leb128)를 `num_files` 다음에 둔다.
- 엔트리 번호는 루트(0), 디렉터리(1..d), 파일(d+1..d+f), 그리고 특수 엔트리(d+f+1..d+f+s) 순이다.
- `entry_parents`와 `entry_offsets`의 길이는 `d + f + s - 1`이 된다. `file_params`는 그대로 파일에만 있다.
- 특수 엔트리의 `entry` 구조는 다음과 같다.

| Name | Type | Conditions |
|---|---|---|
| `entry_name` | u8[*] ≠ 0 | |
| `entry_name_sentinel` | u8 = 0 | |
| `special_kind` | leb128 | |
| `link_target` | u8[*] ≠ 0 | If `special_kind` = 0 |
| `link_target_sentinel` | u8 = 0 | If `special_kind` = 0 |
| `device_major` | leb128 | If `special_kind` ∈ {2, 3} |
| `device_minor` | leb128 | If `special_kind` ∈ {2, 3} |

| `special_kind` | 노드 |
|---|---|
| 0 | link node |
| 1 | special node, "`fifo`" |
| 2 | special node, "`char-device`" |
| 3 | special node, "`block-device`" |
| 그 밖 | unknown node |

- `link_target`은 `entry_name`과 같은 방식으로 디코딩한다. C1₁₆로 시작하면 "`raw`", 그 밖에는 WTF-8이다. 다만 §4.3의 제약을 적용한다.

> 특수 엔트리를 `file_params` 없는 별도 부류로 둔 이유: 파일 부류에 섞으면 `file_params`가 엔트리 번호로 바로 인덱싱되지 않는다. 디렉터리가 이미 같은 이유로 따로 번호를 받는다.
>
> 대상을 CBOR(optional metadata)에 두지 않은 이유: optional metadata는 "읽는 데 꼭 필요하지 않은 정보"이고 무시할 수 있다. 대상은 노드의 정체 그 자체이다.
>
> 대상을 엔트리 안에 인라인으로 둔 이유: metadata block은 zstd로 통째로 압축되므로 비슷한 대상 경로들끼리 잘 압축된다. 대상만 모은 별도 배열도 가능하지만, 엔트리 안에 두는 쪽이 `entry_offsets`로 바로 찾을 수 있어 단순하다.
>
> 비용은 특수 엔트리가 없는 아카이브에서 헤더의 1바이트뿐이다.

**[확정]** `special_kind`에 모르는 값이 오면 invalid archive가 아니라 **unknown node**가 된다. decompressor는 그 노드에 아무것도 만들지 않고 보고하며, 나머지는 계속 해제할 수 있다. 0–3 말고는 모두 unknown node이다.
> invalid archive로 하면 미래 버전이 만든 아카이브는 하나도 풀리지 않는다. 모르는 종류의 나머지 바이트는 `special_data`(u8[*])로 두고, 길이는 `entry_offsets`로 정한다. 병합에서는 다른 비디렉터리 노드처럼 통째로 바뀌므로 규칙을 새로 정의할 필요가 없었다.

### 5.2 device 번호 [확정]

- **[확정]** major와 minor를 따로 저장한다. OS 고유의 `dev_t` 인코딩을 그대로 저장하지 않는다.
  > 쪼개서 얻는 것은 OS 사이의 호환이 아니다(아래 참조). `dev_t` 하나로 표현하는 정본 형태가 없다는 것이 이유이다.
  >
  > - Linux 안에서조차 인코딩이 여럿이다. old 8:8(16비트), 커널 내부의 12:20, `new_encode_dev`의 32비트 배치, glibc의 64비트 배치가 있다. 하나를 고르는 순간 사실상 (major, minor) 쌍을 정의하는 셈이다.
  > - `mknod`에 넘기는 값은 어느 OS에서나 `makedev(major, minor)`로 만든다. 다른 포맷도 쌍으로 저장한다: ustar devmajor/devminor, cpio newc rdevmajor/rdevminor, ZIP 0x000d의 u32 두 개. 반례인 cpio odc/bin은 `c_rdev`를 raw로 저장해 이식성이 깨졌고, newc에서 쌍으로 바뀌었다. 쌍이면 변환기가 출처 OS의 인코딩을 몰라도 손실 없이 옮길 수 있다.
  >
  > `dev_t`에서 major/minor를 나누는 방식은 OS마다 다르다.
  >
  > | OS | `dev_t` | major | minor |
  > |---|---|---|---|
  > | Linux(glibc) | 64비트 | 비트 8–19, 44–63 (32비트) | 비트 0–7, 20–43 (32비트) |
  > | Linux(커널) | 32비트 | 12비트(0–4095) | 20비트 |
  > | macOS | 32비트(부호 있음) | 비트 24–31 (8비트) | 비트 0–23 (24비트) |
  > | FreeBSD 12+ | 64비트, 바이트 배치 `MMMmmmMm` | 32비트 | 32비트 |
  > | FreeBSD 11 이하 | 32비트 | 비트 8–15 (8비트) | 나머지 24비트 |
  > | NetBSD | 32비트 | Linux 커널의 인코딩과 같다 | |
  > | OpenBSD | 32비트 | 비트 8–15 (8비트) | 비트 0–7, 16–31 (24비트) |
  >
  > Linux glibc는 32비트 major/minor를 표현하지만 커널은 12/20비트까지만 쓴다. 범위가 OS마다 다르므로 leb128로 저장한다. decompressor는 해제할 OS가 표현할 수 없는 번호를 만나면 실패하거나 보고해야 한다.
- 번호의 의미는 OS에 따라 다르다. 같은 (major, minor)가 다른 OS에서는 다른 장치이고, OS 사이에 번호로 대응하는 것은 없다. 대응이 있다면 이름과 의미 수준(`/dev/null`은 어디에나 있다)뿐이다.
  > | 장치 | Linux | macOS 27 |
  > |---|---|---|
  > | `/dev/null` | c 1:3 | c 3:2 |
  > | `/dev/zero` | c 1:5 | c 3:3 |
  > | `/dev/random`, `/dev/urandom` | c 1:8, 1:9 | c 17:0, 17:1 |
  > | `/dev/tty` | c 5:0 | c 2:0 |
  > | `/dev/console` | c 5:1 | c 0:0 |
  > | 첫 디스크 | b 8:0 (`sda`) | b 1:0 (`disk0`) |
  >
  > Linux 열은 `Documentation/admin-guide/devices.txt`, macOS 열은 직접 `ls -l`로 확인했다. macOS의 `/dev/console`이 0:0이라 overlayfs whiteout과 번호가 같다.
  >
  > - Linux는 정적 레지스트리(`devices.txt`)를 가진 사실상 유일한 OS이다. 그래도 동적 대역(misc 10:x의 동적 minor, 동적 major, NVMe 등의 259 확장 블록 번호)은 호스트나 부팅마다 다를 수 있다.
  > - macOS(XNU)는 major를 `cdevsw_add`로 동적으로 받는다. devfs 바깥에 장치 노드를 만들 일이 거의 없다.
  > - FreeBSD는 5.x부터 devfs가 번호를 관리하고 block device를 없앴다. devfs 바깥에 `mknod`로 만든 장치 노드는 동작하지 않는다 *(확인 필요)*.
  > - illumos/Solaris의 major는 시스템마다 `/etc/name_to_major`로 정해진다 *(확인 필요)*.
  > - Windows에는 대응하는 개념이 없다.
  >
  > 따라서 아카이브에 든 device node가 실제로 의미를 갖는 경우는 사실상 "Linux에서 나온 번호를 Linux에서 푸는 것" 하나이다. tar도 같아서, Linux의 `/dev`를 tar로 묶어 macOS에서 풀면 엉뚱한 노드가 생긴다.
- **[확정]** 출처 OS는 모델에도 extra에도 기록하지 않는다(§8의 4번). 명세에는 "번호의 해석은 해제하는 플랫폼에 맡긴다"는 비규범 note만 둔다.
  > - 변환할 때 채울 수 없다. tar와 cpio에는 출처 OS 정보가 없다. ZIP "version made by"의 host 바이트는 Unix 계열끼리 구분하지 못한다. 그래서 필드는 대부분 "unknown"이 된다.
  > - 채워진다 해도 다른 OS의 decompressor가 할 수 있는 일은 거부뿐이다. 그것은 §6.4의 기본값(만들지 않고 보고한다)이 이미 한다.
  > - 위험도 늘지 않는다. 엉뚱한 번호의 노드는 명시적으로 요청했을 때만, 그것도 root로 풀 때만 생긴다.

### 5.3 optional metadata [확정]

```cddl
aqr-entry-extra = {
  ...
  ? &(link-group: -7) => int       ; Files sharing a value were the same file (hard links)
  ...
  ? &(win-link-type: -60) => &(file: 0, directory: 1, junction: 2)  ; Link nodes only
  ...
}
```

**`link-group`** (file node에만):

- 값이 같은 file node들은 원래 같은 파일(inode)이었음을 뜻한다.
- **[확정]** 0 이상이면 segment 안에서만 같은 그룹이고, 음수면 아카이브 전체(segment를 넘어 공유)이다. `st_ino`를 그대로 넣으면 segment-local이 된다.
  > 처음에는 반대(0 이상이 전역)였다. `st_ino`를 그대로 넣으면 전역이 되게 하려는 것이었다. 검토해 보니 inode 번호는 재사용되고 `st_dev` 안에서만 유일하다. 그래서 나중에 덧붙인 segment가 `st_ino`를 그대로 쓰면 앞 segment의 무관한 파일과 묶일 수 있고, 내용까지 같으면(빈 파일 등) decompressor가 감지하지 못한다. 손쉬운 길이 위험한 쪽이 되므로 뒤집었다. segment를 넘는 그룹(`recovery-design.md` §5.1의 주기적인 segment 분할, 덧붙이기)은 compressor가 일관성을 책임지고 음수로 명시한다.
  >
  > CBOR `int`는 0..2⁶⁴−1과 −1..−2⁶⁴를 모두 담으므로 64비트 `st_ino`가 그대로 들어가고 음수 쪽도 범위가 같다. 여러 파일 시스템을 담는 compressor는 여전히 일부를 다시 번호 매겨야 한다(ReFS의 128비트 파일 ID도 그렇다).
  >
  > 모델의 최종 값에서 segment-local 그룹을 구분하려면 segment 정보가 필요하다. 그래서 `file node`에 **link group** 항목(전역 정수, 또는 (segment, 음수) 쌍)을 두고, link group의 일대일 이름 바꾸기만 다른 archive value는 같다고 정의했다.
- 같은 그룹의 모든 노드는 내용이 같아야 한다(MUST). compressor는 이들이 같은 leaf block을 가리키게 하는 것이 좋다(SHOULD).
- 같은 그룹의 노드들은 `link-group`과 `comment`를 뺀 나머지 extra가 같아야 한다(SHOULD).
  > mode, uid, 시각은 inode의 속성이므로 원래 같다. 그래도 MUST로 하지 않는 이유: 아래의 병합 규칙 때문에 그룹의 일관성은 최종 값에서만 판단할 수 있다.
- 그룹 일관성은 최종 `archive value`에서 판단한다. 내용이 서로 다른 노드가 같은 그룹에 있으면(예: 나중 segment가 그룹의 한 노드만 바꾼 경우) 그 그룹은 힌트로서 무효이다. invalid archive는 아니다.
  > invalid archive로 하면 segment 하나만 읽는 partial access에서 감지할 수 없는 오류가 생긴다. 힌트로 두면 decompressor가 링크를 만들기 전에 내용 일치를 확인하기만 하면 된다. 같은 leaf block을 가리키는지로 확인하면 비용도 없다.
- 디렉터리의 hard link(일부 macOS Time Machine 볼륨)는 표현하지 않는다.

**`win-link-type`** (link node에만): Windows에서 해제할 때 만들 링크 종류이다. 없으면 decompressor가 정한다(§6.4).

키 영역은 명세에 적었다. −1..−59는 플랫폼 중립, −60대는 Windows, −70대는 Unix, −80대는 macOS, −90대는 그 밖의 플랫폼이다.

## 6. 기존 절과의 상호작용

### 6.1 segment 병합 [확정]

§Merging segments의 규칙은 그대로 동작한다. "둘 다 디렉터리면 병합하고, 아니면 통째로 바꾼다"이므로 link node와 special node는 file node처럼 다뤄진다.

- 디렉터리를 나중 segment의 link node로 바꾸면 하위 노드도 모두 사라진다. 그 다음 segment가 같은 이름의 디렉터리를 다시 만들면 디렉터리로 바뀐다.
- 따라서 **최종 값에서 link node 아래에는 아무것도 없다.** 모델 안의 어떤 경로도 링크를 지나지 않는다(원칙 2).
- §Unencrypted Archive의 "같은 경로의 엔트리가 여러 개 있으면 안 된다(SHOULD NOT)"는 note가 "파일에만 해당한다"고 말하는 부분은 "디렉터리가 아닌 엔트리에만 해당한다"로 고친다.

### 6.2 partial access [확정]

- §Partial access의 note에서 "until the path or one of its ancestors is found as a file"을 "as a non-directory entry"로 고친다.
- 모델 안에서 경로를 찾는 것은 `lstat`의 의미이다. 링크를 따라가지 않는다.
  > 도구가 `cat a/b`처럼 링크를 따라가는 조회를 제공할 수는 있다. 그것은 도구의 일이다. 그럴 때는 따라간 횟수에 상한을 둬야 한다(Linux의 `MAXSYMLINKS`는 40이다).

### 6.3 compressor [확정]

- 기본적으로 symlink를 따라가지 않아야 한다(SHOULD). 따라가는 옵션(`tar -h`에 해당)을 줄 때 순환 감지는 compressor의 몫이다.
- hard link를 감지하면(`st_nlink > 1`, 같은 `(st_dev, st_ino)`) `link-group`을 붙인다. 아카이브 대상 트리 바깥에 있는 링크는 알 수 없으므로 그룹이 하나뿐인 노드가 생길 수 있다. 그런 그룹은 쓰지 않는 것이 좋다(SHOULD NOT).
- 기존 아카이브에 segment를 덧붙일 때, 기존 그룹에 새 링크를 넣으려면 기존 그룹 번호를 알아야 한다. 이미 있는 metadata block을 읽어야 한다는 뜻이다. 그렇게 하지 않는 compressor는 segment-local 번호(0 이상)를 쓴다. 그러면 기존 그룹과 묶이지 않을 뿐, 틀린 결과는 나오지 않는다. 한 번의 실행에서 segment를 주기적으로 나눌 때는 compressor가 번호를 모두 알고 있으므로, segment를 넘는 그룹에 음수를 쓰면 된다.

### 6.4 해제 [확정]

§Extraction에 다음을 추가한다.

- **symlink를 통한 쓰기.** 모델 안에서는 어떤 경로도 링크를 지나지 않는다(§6.1). 그러나 해제 중에 만든 링크는 이미 있던 링크와 같은 위험을 만든다. 기존 규칙 "MUST NOT create, modify or remove any file outside the destination directory, including through symbolic links …already present"를 "…or created during the extraction"으로 넓힌다.
  > 고전적인 tar 공격(`a -> /etc` 다음에 `a/passwd`)은 Aqueous 모델에서는 표현할 수 없다. 남는 경로는 대소문자 구분이 없는 파일 시스템에서 `A`(링크)와 `a/x`(파일)가 충돌하는 경우이다(git CVE-2021-21300이 이 유형이다). 이것은 기존의 "같은 파일 시스템 이름으로 매핑되면 실패하거나 보고하라"는 규칙이 이미 막는다. 이 규칙을 확장하는 것은 그 보장을 명시하는 것이다.
  >
  > 구현 방법으로는 모든 링크를 마지막에 만들거나, `openat`과 `O_NOFOLLOW`나 Linux의 `RESOLVE_BENEATH`를 쓰는 방법이 있다. 명세는 결과만 요구한다.
  >
  > - 앞의 방법을 쓰는 것이 GNU tar(1.35 `extract.c`)이다. `-P`가 없을 때, 대상이 절대 경로이거나 `..` 성분을 담은 symlink는 즉시 만들지 않는다. 대신 모드 0인 빈 파일을 `O_EXCL`로 만들어 둔다. `..`을 담은 hard link와 자리 표시 파일을 가리키는 hard link도 같다. 해제가 끝나면 자리 표시 파일의 (dev, ino, birthtime)이 그대로인지 확인하고 링크로 바꾼다. 다른 엔트리가 그 자리를 덮었으면 링크를 만들지 않는다. 그래서 링크를 통한 쓰기가 일어나지 않는다.
  > - 뒤의 방법에 가까운 것이 libarchive의 `ARCHIVE_EXTRACT_SECURE_SYMLINKS`이다. 경로의 중간 성분이 디스크의 symlink이면 해제를 거부한다(`ARCHIVE_EXTRACT_UNLINK`가 있으면 그 링크를 지운다). bsdtar는 이것과 `SECURE_NODOTDOT`를 기본으로 켜고 `-P`로 끈다.
- **대상이 바깥을 가리키는 링크.** **[확정]** 절대 경로이거나 대상 디렉터리를 벗어나는 링크는 명시적으로 요청받지 않으면 만들지 않고(MUST NOT) 보고한다(MUST). 요청할 방법을 제공해야 한다(SHOULD).
  > 벗어나는지는 링크의 부모 디렉터리에서 시작해, archive value 안의 다른 link node를 따라가며 판단한다. 어휘적으로만 판단하면 `d/e/l -> ../../q`가 있을 때 `d/e/l/../..`를 `d`로 보지만 실제로는 대상 디렉터리의 부모에 닿는다.
  > 기존 도구의 상황은 다음과 같다.
  > - libarchive의 secure 플래그는 이런 링크를 거부하지 *않는다*. `SECURE_NODOTDOT`와 `SECURE_NOABSOLUTEPATHS`는 엔트리 경로와 hard link 대상만 검사한다. symlink 대상은 검사 없이 `symlink()`에 넘긴다. bsdtar도 경로 재작성을 symlink 대상에는 일부러 적용하지 않는다(`tar/util.c`).
  > - GNU tar도 거부하지 않는다. 만들기를 미룰 뿐이다.
  > - unrar는 절대 경로이거나 안전하지 않은 상대 경로를 가리키는 링크를 기본으로 건너뛴다(`-ola`로 끈다).
  > - 7-Zip은 25.01(CVE-2025-55188 대응)부터 위험한 링크를 기본으로 거부한다. 그 수준은 `-snld[N]`로 조정한다.
- **링크를 만들 수 없는 플랫폼.** 권한이 없거나(Windows는 개발자 모드나 `SeCreateSymbolicLinkPrivilege`가 필요하다) 파일 시스템이 지원하지 않으면(FAT), 엔트리 이름 규칙과 마찬가지로 실패하거나 사용자에게 보고해야 한다(MUST). 조용히 다른 것을 만들면 안 된다(MUST NOT).
  > 대체물로 대상의 복사본을 만드는 것은 보고하는 한 허용된다. 대상이 아카이브 안에 있으면 복사할 수 있다.
- **Windows의 링크 종류.** `win-link-type`이 있으면 따른다. 없으면 대상을 아카이브 안에서 해석해 디렉터리이면 디렉터리 symlink를, 그 밖에는 파일 symlink를 만드는 것이 좋다(SHOULD).
- **hard link.** `link-group`이 같고 내용이 같은 파일들을 hard link로 만들 수 있다(MAY). 만들지 않아도 결과는 올바르다.
- **special node.** 명시적으로 요청받지 않았다면 FIFO와 device를 만들지 않아야 한다(SHOULD NOT). 이것은 `unix-uid`와 같은 취급이다. 만들지 않았으면 보고해야 한다(MUST).
  > device 노드는 root로 해제할 때만 만들 수 있다. 그때 권한이 넓은 block device가 하나 생기면 그것으로 디스크 전체를 읽고 쓸 수 있다. FIFO는 권한 없이 만들 수 있지만 그 자체로는 위험하지 않다. 둘 다 끄는 것은 기본값을 단순하게 하려는 것이다.
- **링크 자신의 메타데이터.** link node의 extra(시각, uid, mode)는 링크 자신의 것이다. 적용한다면 `lutimes`, `lchown` 같은 함수로 적용한다. 대상에 적용하면 안 된다(MUST NOT).

### 6.5 `unix-mode`와 `S_IFMT` [확정]

**[확정]** `unix-mode`는 권한 비트(0o7777, 즉 setuid/setgid/sticky 포함)만 담는다. 파일 형식 비트(`S_IFMT`)를 넣으면 안 된다(MUST NOT). decompressor는 그 비트를 무시해야 한다(MUST).

> 노드 종류가 정본이다(원칙 3). `S_IFMT`가 함께 있으면 둘이 어긋날 수 있다. 이것으로 `zip-extras-design.md` §6의 3번이 해소된다.
>
> 명세의 CDDL은 `uint .le 4095`로 좁혔다. 따라서 그 비트가 있으면 decompressor가 무시하는 것이 아니라 CDDL 위반이다.

### 6.6 ZIP 변환 (`zip-extras-design.md`) [제안]

이 설계가 들어가면 다음과 같이 바뀐다.

- external attributes가 `S_IFLNK`인 엔트리(Info-ZIP 방식)는 link node가 된다. 원래 내용이 대상이다. 내용이 NUL을 담거나 비어 있으면 일반 파일로 두고 경고한다.
- 0x000d와 0x756e는 P에서 **M**으로 바뀐다. 가변 부분의 symlink 대상은 link node로, device 번호는 special node로 간다.
  - APPNOTE §4.5.7에 따르면 0x000d의 가변 부분은 link일 때 "the name of the original file"(NUL 없음, 길이 TSize − 12)이고, device일 때 major와 minor(각각 u32le)이다. hard link와 symlink를 구분하는 필드는 0x000d에 없다. external attributes의 모드(`S_IFLNK`인지)로 구분한다.
  - hard link이면 그 이름을 아카이브 안의 경로로 보고, 그 경로의 엔트리와 같은 `link-group`을 준다. APPNOTE는 이것이 아카이브 경로라고 명시하지 않는다. 이름에 해당하는 엔트리가 없으면 일반 파일로 두고 경고한다.
- `S_IFCHR`/`S_IFBLK`/`S_IFIFO` 모드의 엔트리도 special node가 된다.
- `spec/index.bs` §Optional metadata의 TODO 주석 `UNIX attributes (ZIP extra 0x000d) - symlink target`을 지운다.
- `zip-extras-design.md` §5 표에서 "symlink 대상, hard link, device" 행은 승격된 것으로 처리한다.

### 6.7 tar 등에서의 변환 [제안]

- tar typeflag `1`(hard link): 가리키는 경로의 엔트리와 같은 `link-group`을 주고 내용을 공유한다. 가리키는 엔트리가 일반 파일이 아니면 경고한다.
- tar typeflag `2`, pax `linkpath`: link node가 된다.
- tar typeflag `3`/`4`/`6`: special node가 된다.
- RAR5 redirection record의 Windows symlink와 junction에는 `win-link-type`을 준다. 대상이 디렉터리라는 flag는 symlink의 `file`/`directory` 구분이 된다. hard link(타입 4)는 tar typeflag `1`처럼 처리한다. "file copy"(타입 5)는 대상 엔트리의 내용을 공유하는 독립 파일이 되므로 중복 제거로 흡수된다.
- 7z: attribute에 `0x8000`이 있고 상위 16비트가 `S_IFLNK`이면 내용이 대상인 link node이다. `FILE_ATTRIBUTE_REPARSE_POINT`가 있으면 내용은 reparse data 버퍼이다. symlink나 mount point 태그이면 대상을 꺼내 link node로 만들고, 그 밖의 태그는 범위 밖이다(§7). hard link 정보는 7z에 없다.
- 그 밖의 Windows reparse point(WSL symlink, AF_UNIX, 클라우드 placeholder, dedup 등)는 범위 밖이다(§7).

### 6.8 그 밖

- **formula와 모듈.** 링크 대상은 metadata block에 인라인으로 있으므로 formula와 관계없다. `xform-design.md` §4.1의 "심링크는 아카이브의 것" 경계와 맞는다. git-recons를 모듈 형태로 옮길 때 자체 symlink 목록을 이것으로 대신한다.
- **복구.** 링크 대상은 metadata block 안에 있으므로 mirror block이 함께 보호한다. hard link 그룹이 leaf block 하나를 공유하면 그 블록의 손상이 그룹 전체에 미친다. 이것은 중복 제거 일반과 같다.
- **이름 없는 단일 파일 아카이브**(metadata block 없음)는 영향을 받지 않는다.

## 7. 범위 밖

- socket.
- 디렉터리 hard link.
- 그 밖의 Windows reparse point: mount point(junction과 같은 형식이지만 볼륨 GUID를 가리킨다), WSL symlink, AF_UNIX, 클라우드 placeholder, 데이터 중복 제거.
- macOS Finder alias와 bookmark. 리소스 포크를 가진 일반 파일이므로 xattr 쪽 문제이다.
- overlay/OCI whiteout. OCI는 tar 안에서 `.wh.` 접두 이름의 일반 파일로 표현하므로 모델과 무관하다. overlayfs의 char device 0/0 형식은 special node로 표현은 된다.
- sparse file. 특수 파일이 아니라 일반 파일의 저장 방식 문제이다.

## 8. 열린 질문

1. **[해결]** **FIFO와 device를 지금 정의할 것인가**, 번호만 예약할 것인가. 정의한다(§1.3).
2. **[해결]** **`link_target`의 상한.** 두지 않는다. 플랫폼별 상한은 다음과 같다.
   - Linux VFS: `PATH_MAX`(4096, NUL 포함)이므로 4095바이트이다. 파일 시스템이 더 좁힐 수 있다. ext4는 블록 크기 − 1이고, tmpfs는 페이지 크기 − 1이고, XFS는 1023바이트이다.
   - macOS(APFS): 1023바이트이다(`PATH_MAX` 1024). 직접 시험했다.
   - FreeBSD: 1023바이트이다(`copyinstr`에 `MAXPATHLEN` 1024).
   - Windows: reparse data 전체가 16 KiB(`MAXIMUM_REPARSE_DATA_BUFFER_SIZE`)이다. SubstituteName과 PrintName을 함께 담으면 UTF-16으로 약 8000자이다. 이 값은 구조체에서 계산한 것이고 Microsoft 문서에 적힌 값은 아니다.
   > 모든 플랫폼에서 만들 수 있는 길이는 1023바이트까지이다. 상한을 둔다면 이 값이 후보이다. 두지 않는다면 "해제 시 만들 수 없으면 보고하라"(§6.4)로 충분하다.
3. **[해결]** **모르는 `special_kind`의 처리.** unknown node를 두고, 해제 시 보고하고 그 노드만 거부한다(§5.1).
4. **[해결]** **device 번호의 출처 OS.** 기록하지 않는다. 변환할 때 채울 수 없고, 기록해도 할 수 있는 일이 거부뿐이다(§5.2).
5. **[해결]** **`link-group`의 범위.** 부호로 나눈다. 0 이상은 segment-local, 음수는 전역(§5.3).
6. **[해결]** **link node의 대상을 Windows의 드라이브 문자 절대 경로(`C:/…`)로 둘 때의 이식성.** `\`→`/` 말고는 플랫폼에 맡긴다.
7. **[해결: 기본으로 거부, 옵션으로 허용]** **해제 시 바깥을 가리키는 링크를 기본으로 거부할지.** 이 문서는 "모드를 제공하라(SHOULD)"까지만 요구한다. 기본값을 안전 쪽으로 둘지(unrar, 7-Zip 25.01 이후)는 도구의 결정으로 남긴다. tar 계열은 거부하지 않는다(§6.4).

## 9. Action items

### 조사 (R)

- [x] **R1.** RAR5 redirection record와 7z(p7zip, 7-Zip `-snl`/`-snh`)의 실제 저장 형식을 확인해 §2와 §6.7을 고친다.
- [x] **R2.** PKWARE 0x000d의 가변 부분이 hard link일 때 무엇을 담는지 확인한다(§6.6).
- [x] **R3.** Linux, macOS, FreeBSD의 `dev_t` 비트 배치를 확인한다(§5.2).
- [x] **R4.** GNU tar의 자리 표시 파일 방식과 libarchive의 secure 플래그가 실제로 무엇을 막는지 확인한다(§6.4).
- [x] **R5.** macOS 프레임워크에서 symlink를 복사로 바꿨을 때 코드 서명이 깨지는지 확인한다(§1.1).

### 명세 (S)

- [x] **S1.** §Archive model에 link node, link target을 추가한다(§4.2, §4.3). special node 대신 unknown node를 넣었다.
- [x] **S2.** §Metadata block에 `num_specials`, 특수 엔트리 구조, `special_kind` 표를 추가한다(§5.1).
- [x] **S3.** §Interpreting a segment에 특수 엔트리의 해석 단계를 추가한다.
- [x] **S4.** §Merging segments, §Unencrypted Archive의 note, §Partial access의 note에서 "file"을 "non-directory"로 고친다(§6.1, §6.2).
- [x] **S5.** §Extraction의 Issue를 지우고 §6.4의 규칙을 넣는다.
- [x] **S6.** 여러 `file_param`이 같은 leaf block을 가리킬 수 있다고 명시한다(§0).
- [x] **S7.** §Optional metadata에 `link-group`과 `win-link-type`을 추가하고, `unix-mode`를 권한 비트로 좁힌다(§5.3, §6.5). 0x000d 관련 TODO 주석을 지운다.
- [ ] **S8.** `zip-extras-design.md`의 §3.4, §5, §6의 3번을 이 문서에 맞춘다(§6.6).
- [x] **S9.** §Archive model에 special node를, §Metadata block의 `special_kind` 표에 1–3과 `device_major`/`device_minor`를 추가한다(§4.2, §5.1, §5.2). 번호 해석에 대한 note를 넣고, §Extraction의 FIFO/device Issue를 §6.4의 special node 규칙으로 바꾼다.

### 구현 (I)

- [ ] **I1.** compressor에서 symlink, hard link(`link-group`과 leaf block 공유), FIFO, device를 수집한다.
- [ ] **I2.** decompressor의 해제 경로에 §6.4의 규칙을 넣는다. 대소문자 구분이 없는 파일 시스템에서 링크와 디렉터리가 충돌하는 시험을 포함한다.
- [ ] **I3.** git-recons를 모듈 형태로 옮길 때 자체 symlink 목록을 link node로 대신한다.

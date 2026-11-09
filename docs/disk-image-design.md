# 디스크 이미지: 파일 시스템과 가상 디스크

디스크 이미지(가상 머신의 디스크, `dd`로 뜬 원시 이미지, DMG, 메모리 카드와 펌웨어의 이미지)를
그 안의 파일 내용과 나머지로 나눈다. aqr-iso가 ISO 9660과 UDF에 하는 일을 주요 파일 시스템과
그것을 싸는 컨테이너로 넓히는 계획이다(2026-11-09). `file-formats.md`의 "Operating systems and
distribution" 표에서 VMDK·VHD·VHDX·qcow2·VDI, 파일 시스템 이미지, DMG 줄이 이것이다.

## 목표

- 이미지 안의 각 파일 내용이 아카이브의 내용물(content)이 되어, 그 형식의 재압축기(PE는 aqr-exe,
  PNG는 aqr-png, …)를 거치고 같은 아카이브의 다른 파일과 중복 제거된다. 이것이 이득의 거의 전부다.
  이미지 자체의 메타데이터를 줄이는 것은 그다음이다.
- 이미지는 바이트 단위로 그대로 돌아온다. 파서가 이해하지 못하는 것은 모두 나머지(`rest`)에 남으므로,
  손상되었거나 마운트된 채 뜬 이미지, 알 수 없는 판이라도 실패하지 않고 덜 줄어들 뿐이다.
- 수백 GB의 이미지에서도 메모리가 이미지 크기를 따라 늘지 않는다(`streaming-encoder-design.md`의
  원칙을 따른다).

범위 밖: 암호화된 볼륨(BitLocker, FileVault·APFS 암호화, LUKS; 알아보면 원시로 둔다), 공개 명세가
없는 것(ReFS), 지나치게 복잡한 것(ZFS; 수요를 보고 다시 정한다).

## 지금 있는 것

- **aqr-iso**: 디렉터리 트리를 읽어 파일과 그 extent(이미지 안의 바이트 구간)를 만들고(`files`), 겹치지
  않게 늘어놓아(`pieces`) 공식을 만든다(`formula::build`). 루트는 조각의 연결이고, 조각마다 파일 내용의
  스트림이거나 `iso/rest`의 일부다. 모듈은 없다. aqr-compress는 이것을 "파일을 묶기만 하는 아카이브"로
  다루어(`bundle.rs`) 내용물을 병렬로 만들고, 입력 매핑을 공유해 복사하지 않는다.
- **aqr-wim**: 리소스 단위의 LZX 재구성(aqr-lzx). 이미지 안의 `install.wim`은 aqr-iso가 파일로 꺼낸다.
- 압축 컨테이너에 쓸 재구성기: aqr-flate(zlib·deflate 청크), aqr-bvx(LZFSE·LZVN), aqr-bz2, aqr-lz4,
  aqr-lzma, aqr-zstd, aqr-lzx.

aqr-iso의 공식은 그대로 쓸 수 없는 곳이 두 군데다.

1. **조각난 파일.** aqr-iso는 조각(piece)마다 스트림을 하나씩 만든다. CD에서는 파일이 거의 연속이라
   문제가 없지만 FAT·NTFS·ext4에서는 한 파일이 수십 조각이고, 조각마다 따로 재압축되면 PNG나 PE는
   형식으로 알아볼 수조차 없다. 파일마다 스트림 하나를 두고 루트의 조각은 그 스트림의 `Slice { start,
   len }`으로 가리켜야 한다.
2. **파일 수.** 시스템 볼륨에는 파일이 수십만에서 백만이다. 파일마다 스트림을 두면 공식이 무거워진다.
   어차피 클래스별로 병합되므로(`Formula::merged`), 작은 파일(예: 64 KiB 미만)은 처음부터 그 클래스의
   스트림 하나에 이어 붙이고 큰 파일만 따로 둔다. 문턱은 재서 정한다.

## 구조: 세 층, 하나의 평탄한 공식

```
컨테이너 (VHD, VHDX, VMDK, qcow2, VDI, DMG, 원시)  가상 디스크 오프셋 → 파일 오프셋
  └ 파티션 표 (MBR·EBR, GPT, APM)                   파티션 → 디스크 구간
      └ 파일 시스템 (FAT, exFAT, NTFS, ext4, APFS, HFS+, ISO, UDF, …)  파일 → 블록 extent
```

각 층은 주소 변환일 뿐이다. 파일 시스템 파서는 파티션 안의 블록 extent를 내고, 파티션 표와 압축하지
않은 컨테이너의 블록 표가 그것을 이미지 파일 안의 바이트 구간으로 옮긴다. 이렇게 합성하면 공식은
층과 상관없이 aqr-iso와 같은 꼴, 즉 이미지 파일 바이트의 조각 연결 하나가 된다. 컨테이너의 블록 표,
파티션 표, 파일 시스템의 메타데이터는 모두 `rest`로 간다. 가상 디스크의 블록 경계에서 끊기는
extent는 조각이 늘 뿐이다.

압축된 컨테이너(qcow2의 압축 클러스터, streamOptimized VMDK(OVA), DMG의 UDZO·ULFO·UDBZ, E01)만
두 단계다. 청크마다 재구성기의 적용으로 컨테이너를 다시 만들고, 그 입력인 가상 디스크를 다시
파일과 나머지의 연결로 만든다. 이것은 뒤 단계로 미룬다(아래 6단계).

### 크레이트

- **`codecs/aqr-disk`** (새로): 층 사이의 공통 부분. 파일과 extent의 표현, 조각 배치(aqr-iso의 `pieces`를
  옮겨 일반화), 공식 만들기(파일마다 스트림 하나, 작은 파일의 묶음, `disk/rest`), 주소 변환의 합성,
  파티션 표, 압축하지 않은 컨테이너, 탐지. `FORMAT.md`는 여기 하나에 둔다(스트림의 뜻이 파일 시스템마다
  같으므로).
- **파일 시스템마다 크레이트 하나**: `aqr-fat`(FAT12/16/32, exFAT), `aqr-ntfs`, `aqr-ext4`(ext2/3/4),
  `aqr-apfs`, `aqr-hfs`(HFS+, HFSX). 각각 파일과 extent만 낸다. 서로 독립이라 하위 에이전트에 나눠 줄 수
  있다. aqr-iso는 aqr-disk의 공식을 쓰도록 옮기고, 그 `FORMAT.md`는 aqr-disk를 가리킨다.
- aqr-compress: `detect.rs`에 `Format::Disk` 하나(파서 하나라도 받으면), `bundle.rs`의 `is_iso`를
  `disk/rest` 클래스까지 넓힌다. `archive.rs`의 `member_spans`(다른 세그먼트와의 중복 판단)도 같다.

## 풀어야 할 것

### 크기

- 입력은 이미 매핑되고(`store::known`), 내용물은 그 구간을 공유한다. 수백 GB에서도 이것으로 충분한지,
  스필과 resident를 `--stats`로 확인한다.
- 원시 이미지는 흔히 sparse 파일이다. 매핑을 읽으면 구멍이 0으로 읽히므로 맞지만 느리다.
  `SEEK_HOLE`/`SEEK_DATA`(Windows는 `FSCTL_QUERY_ALLOCATED_RANGES`)로 구멍을 알고 그 구간을 영 조각으로
  돌리는 것을 검토한다.
- **영 구간.** 빈 공간과 sparse 구멍은 대개 0이다. 첫 판은 `rest`에 넣고 zstd에 맡긴다. 크기는 거의 0이고,
  드는 것은 0을 압축하고 풀고 해시하는 시간이다. 영 채움 노드(길이만 갖는 노드)는 미룬다. 노드를
  제대로 쓰려면 값을 받는 쪽이 모두 구멍을 알아야 하기 때문이다.
  - 평가기의 값 조각(`eval.rs`의 `Chunk`)에 `Zeros(n)` 같은 종류가 생겨야 한다. 그걸 이용할 수 있는
    소비자는 쓰는 쪽(sparse로 출력), 내용 해시(`Ch`는 0의 해시를 배로 늘려 O(log n)에 낸다), 스트림
    병합과 zstd 정도다. 모듈의 입력(창이든 통째든)은 0을 채워 넘길 수밖에 없다. 그러면 지금보다 나빠지지는
    않지만 얻는 것도 없다.
  - 스트림은 데이터라 구멍을 품을 수 없다. 그래서 내용물 스트림 안의 0(파일 시스템 안의 sparse 파일)을
    노드로 하려면, 그 내용물의 공식을 따로 만들어야 한다.
  - 공식은 바이트만의 함수여야 한다(아카이브는 결정적이고, 명세는 sparse 여부를 기록하지 말라고 한다).
    따라서 노드의 자리는 내용으로 정해야 한다(예: 4 KiB에 맞춘 0 블록). `SEEK_HOLE`은 이미 0인 줄 아는
    구간을 건너뛰는 데에만 쓸 수 있고, 데이터 구간은 여전히 훑어야 한다.
  - 가장 큰 이득인 sparse 출력은 노드 없이도 얻는다. 해제할 때 쓰는 쪽이 0 블록을 직접 알아보면 된다
    (`cp --sparse=auto`처럼). 이것은 디스크 이미지만이 아니라 모든 파일에 듣는다.
  - 0단계에서 얇은 VM 디스크(대부분 0)로 0에 드는 압축·해제·해시 시간을 잰다. 전체 시간에서 그 몫이
    크게 나올 때만 노드를 위의 설계로 다시 검토한다.
- 0이 아닌 빈 공간(지운 파일의 잔재)은 `rest`에 그대로 둔다. 아카이브의 장거리 일치가 잡을 수 있으면
  잡는다.

### sparse 출력의 비용 (2026-11-09)

macOS: M4, APFS, 디스크 94% 참.

데이터와 0 구간을 번갈아 쓴 파일을 0까지 다 쓴 것(dense)과 구멍으로 만든 것으로 비교했다(쓰기와 `fsync`,
읽기, `unlink`; 벽시계라 대략이다).

- **`lseek`로 건너뛰면 APFS는 파일의 처음 약 512개 구멍을 크기와 상관없이 0으로 채워 할당한다.**
  4 MiB 구멍 600개에서 2.18 GB가 할당되었고, `ftruncate`로 크기를 먼저 잡아도 같았다. 쓴 뒤
  `fcntl(F_PUNCHHOLE)`로 뚫으면 정확히 구멍이 된다(4 KiB까지).
- 구멍 하나의 비용(`F_PUNCHHOLE`, 16 KiB 구멍, 파일 2 GiB 이하)은 쓰기와 `fsync`를 합쳐 약 25 µs다.
  구멍 4096개에서 65536개까지 거의 선형이었다. `fsync`가 그중 약 15 µs인데, 메타데이터를 쓰는 몫이다.
  `unlink`도 구멍 하나에 3 µs쯤 더 든다.
- 0을 쓰는 속도는 페이지 캐시에서 약 2.5–3 GB/s이고, 더러운 페이지가 한도를 넘으면 0.5–0.8 GB/s다.
  따라서 시간만으로 본 손익분기는 구멍 25–75 KB 정도다. 다만 sparse의 주된 이유는 디스크 공간이다.
- 페이지가 16 KiB라 그보다 작은 구멍은 나쁘다. 4 KiB 구멍 26만 개 파일은 읽기가 5.8 s였다(dense는 0.1 s).
  구멍을 뚫으면 이웃 데이터의 페이지가 무효가 되어, 작은 extent들을 디스크에서 다시 읽는다.
- 큰 파일에서는 갑자기 나빠졌다. 4 GiB 파일, 16 KiB 구멍 131072개에서 쓰기가 28–32 s였다(65536개, 2 GiB
  파일에서는 1.5 s). 4 KiB 구멍 262144개, 2 GiB 파일에서는 4 s였으므로 개수만의 문제는 아니다. 더러운 데이터의
  양과 쓰기 지연이 함께 작용하는 것으로 보이지만 아직 가르지 않았다.

Linux(ext4, Lima VM, 가상 디스크가 호스트 APFS 위의 파일이라 대략)와 Windows(NTFS, 4 KiB 클러스터,
NVMe; 측정 폴더는 Defender 검사에서 뺐다)에서도 같은 프로그램으로 쟀다. 처음에는 Defender가 켜진 채 재서 작은 구멍의 읽기가
부풀었다. 아래는 검사에서 뺀 뒤의 값이다.

| | 구멍의 단위 | 구멍 만들기 | 구멍 하나의 비용(쓰기와 flush) | 시간의 손익분기 |
|---|---|---|---|---|
| APFS | 4 KiB(16 KiB 미만은 읽기가 나쁘다) | `F_PUNCHHOLE`만 확실하다(`lseek`는 처음 약 512개를 0으로 채운다) | 약 25 µs | 25–75 KB |
| ext4 | 4 KiB, `lseek`로도 정확하다 | `lseek`, `fallocate(PUNCH_HOLE)` 같다 | 4 KiB 구멍 약 55 µs, 16–64 KiB 구멍 10–40 µs(`fsync`의 몫) | 약 64 KiB |
| NTFS | **64 KiB에 맞춘 64 KiB**: 그보다 작거나 어긋난 구멍은 `FSCTL_SET_ZERO_DATA`로도 할당이 그대로다 | `FSCTL_SET_SPARSE` 뒤 파일 포인터로 건너뛰기와 `FSCTL_SET_ZERO_DATA`가 같다 | dense 대비 45–55 µs. 실제로 생긴 구멍이 많은 파일은 순차 읽기도 구멍당 약 40 µs 느리다(65536개: 3.3 s 대 0.85 s) | 150–300 KB(0을 약 3 GB/s로 쓰므로) |

- NTFS에서 구멍이 하나도 생기지 않는 경우(64 KiB 미만)에도, sparse로 표시하고 건너뛰면 flush가 느려진다(구멍 65536개:
  3–6 s 대 0.5 s). 읽기는 dense와 같다. 그러므로 실제로 구멍이 생길 때만 sparse로 표시한다.
- 1 MiB 구멍은 세 곳 모두 확실히 이득이다(NTFS, 4096개: 쓰기와 flush 0.4 s 대 1.9 s; 읽기는 0.8 s 대 0.5 s).

정책 안: 0 블록을 64 KiB 단위(오프셋도 64 KiB에 맞춘다)로만 알아보고, 같은 구멍으로 이어지는 0 블록이 256 KiB 이상일 때
구멍으로 한다. 이렇게 하면 NTFS의 단위와 APFS의 페이지에 맞고, 세 곳 모두에서 손익분기를 넘는다. macOS는
`F_PUNCHHOLE`, Linux는 `lseek`, Windows는 첫 구멍에서 `FSCTL_SET_SPARSE`를 하고 건너뛴다. 파일 하나의 구멍이
수만 개를 넘으면 문턱을 올린다. 디스크 공간만 보면 이것보다 작은 구멍도 이득이므로, 문턱을 옵션으로 둘 수 있다.

### 조각과 해제 순서

루트는 이미지 순서로 조각을 잇고, 조각은 파일 스트림의 중간을 가리킨다. 조각난 파일이 많으면 디코더는
스트림을 차례에 맞지 않게 읽는다. 평가기의 차례 밖 출력 spool이 이것을 받지만, 디스크 사용과 시간을
조각화가 심한 NTFS에서 재 본다. 나쁘면 큰 파일의 조각 순서대로 스트림을 쓰는 것(파일 하나를 조각마다
나누되 같은 내용물로 묶는 것)을 검토한다.

### 메타데이터

처음에는 모두 `rest`다. 크기를 재고 큰 것부터 다룬다.

- NTFS의 `$MFT`: 파일당 1 KiB 레코드이고 섹터마다 fixup(update sequence)이 있다. 백만 파일이면 1 GB다.
  fixup을 풀어 레코드를 필드별로 나누는 것(`recs-fields`)이 가장 클 것이다.
- ext4의 inode 표와 블록 비트맵, 그룹 디스크립터(체크섬 포함): 비트맵과 체크섬은 유도할 수 있다.
- APFS의 객체마다 있는 Fletcher-64 체크섬, GPT의 CRC-32: 유도할 수 있다.
- 이것들은 모듈이 필요한 일이다. 첫 판은 모듈 없이 간다.

### 공유 extent와 이상한 배치

하드 링크, APFS 클론, btrfs·XFS의 reflink, 스냅숏은 한 extent를 여러 파일이 가리킨다. aqr-iso처럼 앞의
파일이 갖고 겹치는 것은 버린다(먼저 온 것이 이긴다). 메타데이터가 어긋난 이미지(dirty, 마운트된 채
뜬 것)는 extent가 이미지 밖이거나 서로 겹친다. 파서는 그런 extent를 버리고 계속한다.

### 파일 시스템 안의 압축

압축된 파일은 extent가 압축된 바이트를 가리키므로, 첫 판에서는 그 형식 그대로 내용물이 된다(종류
`Unknown`). 다음에 재구성기를 붙인다.

| 압축 | 어디서 | 재구성기 |
|---|---|---|
| LZNT1 | NTFS 압축 속성 | 새로(MS-XCA 명세), aqr-lzo처럼 파스의 기록 |
| XPRESS(Huffman), XPRESS4K/8K/16K, LZX | Windows 10 이후의 WOF(CompactOS, `compact /exe`), 시스템 볼륨에 흔하다 | XPRESS는 새로(MS-XCA), LZX는 aqr-lzx |
| zlib, LZVN, LZFSE | APFS·HFS+의 decmpfs(시스템 파일 대부분) | aqr-flate, aqr-bvx |
| gzip, xz, lz4, zstd | squashfs, EROFS, btrfs | aqr-flate, aqr-lzma, aqr-lz4 |

WOF와 decmpfs는 시스템 볼륨에서 큰 몫이라 각 파일 시스템의 첫 판 다음 순서로 둔다. WOF는 재분석
지점(reparse point)과 `WofCompressedData` 대체 스트림에, decmpfs는 확장 속성이나 리소스 포크에 있다.

## 단계

| 단계 | 내용 | 시험 자료 | 이유 |
|---|---|---|---|
| 0 | aqr-disk: aqr-iso의 공식 일반화(파일마다 스트림, 작은 파일 묶음), MBR·GPT·APM, 원시 이미지, aqr-compress 연결. aqr-iso를 옮기고 그 결과가 같은지(또는 나은지) 확인 | 기존 aqr-iso fixture, GPT 위의 FAT | 모든 단계의 바탕 |
| 1 | FAT12/16/32, exFAT | `newfs_msdos`, `newfs_exfat`, `hdiutil`(macOS) | 가장 단순해 공식을 검증하기 좋다. SD 카드, USB, EFI 시스템 파티션 |
| 2 | NTFS: `$MFT`의 런리스트, 상주 데이터(`rest`), 대체 데이터 스트림, sparse | Windows(피어 세션)의 `format`, VHDX | Windows VM·백업 |
| 3 | ext2/3/4: extent 트리와 간접 블록, inline data, flex_bg, 64bit | Lima VM의 `mke2fs` | Linux VM, Raspberry Pi와 Android 이미지 |
| 4 | 압축하지 않은 컨테이너: VHD(fixed, dynamic, differencing은 원시), VHDX, VMDK(monolithicSparse, flat), qcow2(압축 클러스터 없는 것), VDI | `qemu-img convert`(Lima), Hyper-V | VM 디스크가 실제 수요의 대부분 |
| 5 | WOF(XPRESS, LZX), LZNT1 | `compact /exe:xpress4k` 등 | Windows 시스템 볼륨의 상당 부분 |
| 6 | APFS: 컨테이너 슈퍼블록, 최신 체크포인트, 객체 맵, 파일 시스템 B-트리의 extent 레코드. 스냅숏은 가장 최근 것만 | `hdiutil create -fs APFS` | macOS 볼륨과 DMG |
| 7 | HFS+, DMG(UDIF): UDRW·UDRO는 4단계처럼, UDZO·ULFO·UDBZ는 청크 재구성 뒤 안쪽 디스크 | `hdiutil` | macOS 배포물. 압축 DMG는 두 단계 공식의 첫 사례 |
| 8 | 압축 컨테이너: qcow2 압축 클러스터, streamOptimized VMDK·OVA, E01 | `qemu-img -c`, OVA 내보내기 | 7단계의 틀 재사용 |
| 9 | decmpfs, 그 밖: XFS, btrfs, squashfs, EROFS, F2FS, Android sparse, 원시 CD 섹터(`other-projects.md` §7) | Lima VM | 수요를 보고 |

각 단계는 파서, README(무엇을 읽는가, 결과, 시험), 손상·잘린 이미지 시험(aqr-iso의 것처럼), 그리고
`--stats`를 켠 `cargo aqr` 측정(크기, 시간, 힙과 resident 최대)으로 끝난다. 0단계 뒤의 비교 기준은
같은 이미지를 탐지 없이 압축한 결과다.

선택: 파서가 트리를 이미 읽으므로 `aqr -d`가 디스크 이미지를 외부 아카이브로 풀게 할 수 있다
(aqr-unpack). 이름과 시각, 권한까지 읽어야 해서 일이 더 들고, 재압축과는 독립이다.

## 명세의 출처

GPL 코드(Linux 커널의 파일 시스템 드라이버, ntfs-3g, e2fsprogs, VirtualBox, QEMU의 코드)는 읽지 않는다.
문서와 공개 명세만 읽고, 도구는 fixture를 만드는 블랙박스로만 쓴다.

| 대상 | 출처 |
|---|---|
| FAT | Microsoft FAT32 File System Specification(fatgen103) |
| exFAT | Microsoft exFAT file system specification(2019 공개) |
| NTFS | 공식 명세 없음. Linux-NTFS 프로젝트의 문서(문서이지 코드가 아님), libyal의 libfsntfs 문서(`documentation/`만) |
| LZNT1, XPRESS | [MS-XCA] |
| WOF | Microsoft 문서의 `WOF_FILE_COMPRESSION_INFO`, 재분석 태그 |
| ext4 | 커널의 `Documentation/filesystems/ext4/`(문서만) |
| APFS | Apple File System Reference |
| HFS+ | Apple TN1150 |
| MBR, GPT | UEFI 명세 5장 |
| APM | Apple의 Inside Macintosh 문서 |
| VHD | Microsoft Virtual Hard Disk Image Format Specification |
| VHDX | [MS-VHDX] |
| VMDK | VMware Virtual Disk Format 1.1 |
| qcow2 | QEMU의 `docs/interop/qcow2.txt`(명세 문서) |
| VDI | 공식 명세 없음. libyal의 문서 |
| DMG(UDIF) | 공식 명세 없음. 공개된 해설 문서(`koly` 트레일러, `mish` 블록 표) |
| E01 | libyal의 libewf 문서 |

libyal 문서처럼 코드 저장소에 딸린 문서는, 문서만 읽는다고 브리프에 적는다.

## 시험 자료

- 작은 fixture(수 MB)는 스크립트로 만들어 커밋한다: macOS의 `hdiutil`, `newfs_msdos`, `newfs_exfat`,
  `newfs_hfs`, `newfs_apfs`; Lima VM의 `mke2fs`, `mkfs.xfs`, `mkfs.btrfs`, `mkntfs`(블랙박스), `qemu-img`,
  `mksquashfs`; Windows 피어의 `format`, `compact`, `diskpart`, Hyper-V. 파일 몇 개를 조각나게 쓰는 단계
  (작은 파일을 쓰고 지우기를 되풀이)를 넣는다.
- 측정용 실제 이미지(VM 디스크, 시스템 볼륨, DMG)는 크고 사적인 데이터가 섞이기 쉽다. 사용자가 고르고
  재는 것을 원칙으로 하고, 네트워크 드라이브에서 일괄로 읽지 않는다.

## 정할 것

1. 단계의 순서. 위 순서는 단순한 것부터 공식을 검증하고 VM 디스크(2–4)로 가는 것이다. 실제로 가진
   이미지가 DMG나 Raspberry Pi 이미지 쪽이라면 순서를 바꾼다.
2. 영 구간: 노드는 미루고, 0단계에서 잰 뒤 다시 본다. 해제할 때의 sparse 출력(쓰는 쪽이 0 블록을 알아보는
   것)은 노드와 상관없이 따로 할지 정한다.
3. 크레이트 배치(aqr-disk와 파일 시스템마다 하나).
4. `aqr -d`로 이미지를 푸는 것을 범위에 넣을지.

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
   형식으로 알아볼 수조차 없다. 파일마다 스트림 하나를 둔다. 다만 aqr-compress는 루트의 슬라이스
   하나하나를 내용물로 보므로, 루트가 그 스트림을 `Slice { start, len }` 여럿으로 가리키면 조각마다
   다시 따로 재압축된다. 그래서 조각이 둘 이상인 파일은 스트림 전체를 Split 명령으로 조각들로 자르고
   (`aqr_formula::Of::Node`, 명세의 Split은 아무 값이나 받는다), 루트는 그 출력들을 제자리에 둔다.
   내용물은 Split의 입력 하나이고, 재압축된 공식이 그 자리에 들어간다(0단계에서 구현).
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

0단계(2026-11-09): `codecs/aqr-disk`(파티션 표, 파일마다 스트림, 조각난 파일의 Split, `disk/rest`),
aqr-iso를 그 위로 옮김, aqr-compress의 `Format::Disk`. ISO 이미지의 아카이브는 바이트 단위로 같다
(aqr-disk README). 남은 것: 작은 파일의 묶음(파일 수가 많은 실제 볼륨이 생기는 1단계에서 재고 정한다),
`SEEK_HOLE`, 조각난 파일을 실제 파일 시스템으로 끝까지(아카이브까지) 시험하는 것(1단계의 FAT fixture).

1단계(2026-11-09): `codecs/aqr-fat`(FAT12/16/32, exFAT). 조각난 파일은 FAT fixture에서 아카이브까지
시험했다(aqr-compress의 `fat_files_in_pieces`; macOS의 exFAT는 빈틈에 쓰지 않아 조각나지 않았다).
/usr/share의 FAT32 볼륨(734 MB, 파일 20992개)에서 26.21 → 23.34 MB, 6.0 → 7.0 s(aqr-fat README).
- 볼륨을 읽고 공식을 만드는 데 53 ms이므로 **작은 파일의 묶음은 하지 않는다**: 파일마다 스트림을 두는
  비용이 보이지 않았다. 백만 파일 볼륨에서 다시 본다.
- 재 보니 aqr-compress가 내용물을 찾는 데 파일 수의 제곱이 들었다(`contents_apart`를 슬라이스마다
  물었다). 고쳤다(ISO 이미지도 12.7 → 10.3 s).
- macOS의 msdosfs는 1 KiB 클러스터의 4 MiB FAT12에 파일을 잘못 썼다(fixture는 8 MiB로 만든다).
- 조각난 볼륨: 500 MB FAT32에 파일을 채우고 하나 걸러 지운 뒤 다른 파일로 채웠다(185개 파일이 2848조각).
  같은 파일을 조각 없이 담은 볼륨과 크기·시간이 같았다(15.68 / 15.01 MB, 6.2 / 6.5 s). 둘 다 압축 시간의
  절반이 마지막 검증(`verify`, 한 스레드 3.4 s)이고, 그 대부분이 내용 해시(`eval::hashed`)에서 조각의 CRC를
  잇는 데(`checksum_combine`) 든다: 작은 텍스트 파일이 많을 때의 비용이고 조각과는 상관없다. 따로 본다.

2단계(2026-11-09): `codecs/aqr-ntfs`. fixture는 Lima VM의 mkntfs와 ntfs-3g(블랙박스)로 만들고, ntfs-3g가
만들지 않는 것(파일과 MFT 자신의 런이 확장 레코드에 있는 것, 초기화 크기가 데이터보다 짧은 것)은 시험이
볼륨을 직접 만든다. ntfs-3g는 빈틈을 남겨 두어도 연속된 빈 공간에 쓰므로 런이 수백 개인 파일을 만들기
어렵다. 압축·암호화된 스트림, 상주 데이터, 시스템 파일은 `rest`다(LZNT1은 5단계). Windows가 만든
볼륨(`format`, VHDX)은 아직 재지 않았다(피어 세션).
- Windows 피어가 만든 고정 VHD(MBR, Windows가 포맷한 NTFS)에서 런이 384개라 속성 목록이 필요한 파일까지
  맞게 읽었다. WOF 파일은 `WofCompressedData` 스트림으로 내용물이 된다(5단계에서 XPRESS를 푼다).

3단계(2026-11-09): `codecs/aqr-ext`(ext2/3/4: 확장 트리, 블록 맵, meta_bg). fixture는 Lima VM의 mke2fs와
커널 드라이버(루프 마운트)로 만든다. 1.57 GB ext4 볼륨(파일 34599개, 134개가 7297조각)에서 478.3 →
425.7 MB, 31.4 → 53.0 s(aqr-ext README).
- 시간의 절반이 마지막 검증이다(한 스레드 26.6 s). 이미지가 아카이브의 잎 하나이고, 그 안의 적용 수천 개
  (커널 모듈 `.ko.zst` 5848개 등) 가운데 기록이 없는 것을 `eval::hashed`가 하나씩 평가하며 큰 블록을 그때마다
  다시 푼다. 블록 캐시를 4개에서 256 MiB로 늘려도 26.6 → 23.6 s뿐이었다(블록 하나가 수백 MB). 해제는 1.4 s다.
  검증이 잎 안의 값을 블록 순서로 평가하거나, 큰 이미지를 잎 여럿으로 나누는 쪽을 따로 본다. 디스크 이미지
  전부에 해당한다.
- 해제의 힙도 예산을 넘는다(Windows의 3 GB VHD: 3.3 GB). 단일 파일 아카이브는 클래스마다 블록이 128 MiB까지라,
  내용물 종류가 많은 디스크 이미지에서는 동시에 살아 있는 블록이 많다. 스트리밍 인코더 계획(`streaming-encoder-design.md`)의
  "클래스마다 블록 하나"의 몫이므로 거기서 다룬다.
- 그러다 찾은 것: 조각난 파일의 Split 출력을 하나씩 해시하면 앞 조각들을 블록째 붙잡아 힙이 4.24 GB까지
  올랐다(예산 2 GB). 출력을 한 번에 해시하도록 고쳤다(1.62 GB).

4단계(2026-11-09): `aqr_disk::container`(동적 VHD, VHDX, VMDK 희소 extent, qcow2 v2/v3, VDI). 읽는 쪽은
바이트 대신 `Image`(범위로 읽기)를 받도록 바꾸었다: 평범한 이미지는 빌려주고, 컨테이너는 블록 표로
모아 준다(할당되지 않은 블록은 0). 파일의 extent는 컨테이너 안의 위치로 옮긴다. fixture는 Lima VM의
qemu-img(블랙박스)가 만든다. 2 GB 디스크의 VHDX는 원시 디스크와 같은 크기로 줄었다(aqr-disk README).
Windows가 만든 동적 VHD·VHDX는 피어 세션에 맡겼다.

5단계(2026-11-09, 진행 중): aqr-ntfs가 WOF 재분석 지점을 읽어 파일을 압축된 것으로 알리고, aqr-disk가 LZX
파일을 데이터(내용물)와 aqr-lzx의 `lzx-recons` 적용으로 나눈다(1024 청크씩, 32 MiB 이하). Windows 피어가
만든 2 GB VHD(System32 1500 MiB, `compact /exe:lzx`, 빈 공간은 0으로 채움)에서 531.2 → 393.3 MB(-26%).
XPRESS는 MS-XCA로 새로 쓴 aqr-xpress(파스의 기록: Windows의 WOF 4K 청크에서 텍스트 2.98%, 코드 길이는 블록의
빈도로 만든 길이 제한 Huffman 그대로)가 맡는다: 같은 VHD를 `compact /exe:xpress4k`로 만든 것에서 696.2 →
379.8 MB(-45%). LZNT1도 MS-XCA로 쓴 aqr-lznt1(기록 0.35–0.48%: Windows의 인코더는 탐욕적이고, 체인의 첫 두 후보
가운데 고른다)이 맡는다. aqr-ntfs가 압축 단위마다 할당된 클러스터를 알린다. 같은 VHD를 `compact /c`로 만든
것에서 672.8 → 364.2 MB(-46%).
- 검증(`eval::hashed`)은 기록이 알리지 않는 값을 스레드에서 미리 평가하도록 고쳤다(15b29471): LZX VHD 19.6 → 6.4 s,
  ext4 1.5 GB 18.1 → 3.3 s(실행 43.0 → 32.5 s), LZNT1 VHD 실행 169.8 → 73.8 s, 아카이브는 모두 같다.
- 그 다음: LZNT1 VHD에서 미리 평가하는 값은 조각난 파일 953개의 Split뿐이었다(입력의 해시는 기록이 알지만
  조각 경계에서 자를 수 없다). 그 적용들이 여러 블록(작은 것 수천, 클래스의 128 MiB짜리)에서 읽어 블록 넷의 캐시가
  계속 다시 풀었다. 512 MiB까지 두도록 바꾸어 73.8 → 36.3 s(256 MiB: 55.8 s), LZX VHD 52.8 → 46.4 s.
- 남은 것: 조각난 파일의 Split은 여전히 평가한다. 조각 경계의 해시를 사전 검증이 기록하게 하면 없앨 수 있다.
- 처음 VHD는 compact 전의 원본이 빈 공간에 남아 있어(1.4 GB) 원시 압축에 유리했다(844.6 → 706.5 MB).
- `lzx-recons` 적용 하나가 파일 전체면 힙이 예산을 넘고(3.59 GB) 크기도 줄지 않았다(846 MB). aqr-wim처럼 32 MiB씩
  나누니 둘 다 풀렸다.

6단계(2026-11-09): `codecs/aqr-apfs`(Apple File System Reference로 쓴 읽는 쪽: 최신 체크포인트, 컨테이너와 볼륨의
객체 맵, 파일 시스템 B-트리의 디렉터리·inode·extent 레코드, 볼륨 여럿). decmpfs 파일(UF_COMPRESSED)과 암호화·봉인된
볼륨, 스냅숏, xattr는 `rest`다. fixture는 hdiutil(볼륨 둘은 컨테이너 1 GiB 이상: 512 MiB마다 하나). `ditto
--hfsCompression`은 다른 볼륨에서 복사할 때만 압축한다. 1.5 GB 이미지(Rust 툴체인 lib, /usr/share)에서 346.3 →
325.3 MB.

7단계(2026-11-09): `codecs/aqr-hfs`(TN1150으로 쓴 HFS+·HFSX 읽는 쪽: 카탈로그와 extents overflow B-트리, 데이터
포크, 파일 하드 링크). decmpfs 파일, 리소스 포크, 저널은 `rest`다. aqr-disk의 `dmg`(UDIF: `koly`, 속성 목록의
`mish` 표)와, 원시 청크와 0만의 UDIF(UDRO)는 4단계의 컨테이너다. 압축 DMG는 aqr-compress의 공식(`Format::Dmg`)이다:
디스크가 content 하나(그 파일들을 다시 분해)이고, 그것을 청크마다 한 번의 Split으로 자른다. zlib 청크는 데이터
32 MiB마다 flate-set 적용 하나로, 0 청크는 아무것도 아닌 Patch의 원본으로 버린다.
- hdiutil의 zlib 청크는 zlib 1.2.12 레벨 1의 출력과 바이트까지 같다. 기본 프로파일 6의 레코드는 스트림의
  10%(36.7 MB)였고 프로파일 10의 zlib 에뮬레이션은 0.01%다. 청크 하나로 시험해 모든 토큰을 맞히면 그 매개변수를
  힌트로 프로파일 10을 쓴다(검색하지 않아 오히려 빠르다).
- 1.33 GB HFS+ 디스크(Rust 툴체인 lib, 파일 2644개): 원시 이미지 245.5 → 231.7 MB(zstd -3 307.4 MB), 그 UDZO
  371.6 MB → 230.9 MB(전에는 검증이 맞는 공식을 거부해 원시로 367.0 MB), 21.0 s, 풀기 4.8 s.
- UDBZ: bzip2 청크는 aqr-bz2의 `take_whole`로 청크마다 적용 하나(공식 맨 위에 두어 사전 검증과 풀기가 스레드에서
  앞서 한다; Split 뒤의 묶음이면 한 스레드로 차례로 돌아 85 s였다). 재구성하지 못하는 청크는 그대로 두고 디스크의
  그 부분은 버린다. 같은 디스크의 UDBZ(320.6 MB): 264.9 MB(스캔이 찾은 bzip2 스트림), 116 s → 230.7 MB, 41 s,
  풀기 14.6 s. 시간은 대부분 libbzip2 압축기를 두 번(확인, 사전 검증) 도는 것이다.
- ULFO: LZFSE 청크는 aqr-bvx의 `recons::split`(다시 합쳐 확인)로 청크마다 적용 하나, 레코드는 한 스트림.
  한 스레드에 데이터 약 37 MB/s로 빨라 모든 레벨에서 받는다. 같은 디스크의 ULFO(329.8 MB): 327.5 MB(원시) →
  233.4 MB, 0.7 → 25.1 s, 풀기 5.3 s.
- 해시 검증의 버그 둘: Split의 상태를 값 사이에 공유해, 다른 Split의 출력으로 된 값의 Split(조각난 파일이 있는
  디스크를 청크로 자른 것)이 짧다고 실패했다. 이제 값마다 따로 평가한다. 그 평가가 512 MiB 블록 캐시를 디스크
  순서로 오가며 다시 풀어(50 GB) 힙이 19.6 GB였다. 평가마다 블록의 남은 슬라이스를 세어 둔다(36.1 → 21.8 s,
  19.6 → 4.5 GB).
- 남은 것: 검증의 힙(4.4 GB, 예산 2 GB). 청크 경계가 파일 경계와 달라 조각의 해시를 바이트로 만들어야 하므로
  디스크 파일들의 모듈을 다시 돌린다. 사전 검증이 Split 출력의 해시를 기록하면 없앨 수 있다(5단계와 같은 문제).
  작은 DMG는 모듈 바이트(flate-set 72 KB)가 커서 원시로 남는다.

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

# 디스크 이미지: 파일 시스템과 가상 디스크

디스크 이미지(VM 디스크, `dd` 이미지, DMG, 카드·펌웨어 이미지)를 그 안의 파일 내용과 나머지로 나눈다.
0–8단계는 2026-11-09에 구현했고 이 문서는 그 뒤에 남은 설계 근거, 측정, 남은 일만 둔다. 끝낸 일의
경과는 git 이력에 있다. 읽는 쪽의 사양은 `codecs/aqr-disk/{README,FORMAT}.md`와 각 파일 시스템 크레이트의
README에, 표 위치는 `file-formats.md`의 "Operating systems and distribution"에 있다.

## 목표

- 이미지 안의 각 파일 내용이 아카이브의 내용물이 되어 그 형식의 재압축기를 거치고 다른 파일과 중복
  제거된다. 이득의 거의 전부다. 메타데이터를 줄이는 것은 그다음이다.
- 이미지는 바이트 단위로 돌아온다. 파서가 이해하지 못한 것은 모두 `rest`에 남으므로 손상·마운트된 채 뜬
  이미지나 모르는 판은 실패하지 않고 덜 줄어들 뿐이다.
- 수백 GB에서도 메모리가 이미지 크기를 따라 늘지 않는다(`streaming-encoder-design.md`).

범위 밖: 암호화된 볼륨(BitLocker, FileVault·APFS 암호화, LUKS; 알아보면 원시로 둔다), 공개 명세가 없는 것
(ReFS), 지나치게 복잡한 것(ZFS; 수요를 보고).

## 구조 [구현]

세 층(컨테이너 → 파티션 표 → 파일 시스템)은 주소 변환일 뿐이고, 합성하면 공식은 이미지 파일 바이트의 조각
연결 하나가 된다(aqr-iso와 같은 꼴). 컨테이너의 블록 표, 파티션 표, 파일 시스템 메타데이터는 `rest`다.
압축된 컨테이너(qcow2 압축 클러스터, streamOptimized VMDK, DMG의 UDZO·ULFO·UDBZ, E01)만 두 단계다:
청크마다 재구성기의 적용으로 컨테이너를 다시 만들고, 그 입력인 가상 디스크를 다시 파일과 나머지로 나눈다
(`aqr_disk::chunked`, aqr-compress의 `chunked`, `Format::Dmg`).

- `codecs/aqr-disk`: 공통 부분(파일·extent, 조각 배치, 공식, 파티션 표, 컨테이너, 탐지). `FORMAT.md`는 여기 하나.
- 파일 시스템마다 크레이트 하나: aqr-iso, aqr-fat, aqr-ntfs, aqr-ext, aqr-apfs, aqr-hfs. 각각 파일과 extent만 낸다.
- 조각난 파일은 파일마다 스트림 하나를 두고 Split으로 조각들로 잘라 루트가 제자리에 둔다(조각마다 따로
  재압축되는 것을 막는다). aqr-compress: `Format::Disk`, `bundle.rs`, `member_spans`.
- 공유 extent(하드 링크, 클론, 스냅숏)는 먼저 온 파일이 갖고, 이미지 밖이거나 겹치는 extent는 버린다.
- 압축된 파일: WOF(XPRESS·LZX)는 aqr-xpress·aqr-lzx, NTFS의 LZNT1은 aqr-lznt1, 그 밖은 형식 그대로 내용물.

## 단계의 결과 (참조되는 번호를 남긴다)

각 단계 마지막 줄은 그 단계의 측정(`file-formats.md`에 최종 표)이다. 모두 2026-11-09, M4.

- **0**: aqr-disk, aqr-iso를 그 위로 옮김(ISO 아카이브는 바이트까지 같다).
- **1** FAT12/16/32·exFAT: FAT32 734 MB 26.21 → 23.34 MB. 조각난 볼륨(185개 파일이 2848조각)도 조각 없는 것과
  크기·시간이 같다. 작은 파일의 묶음은 하지 않기로 했다(파일마다 스트림의 비용이 보이지 않았다; 백만
  파일에서 다시 본다). 고친 것: aqr-compress의 내용물 찾기가 파일 수의 제곱이었다.
- **2** NTFS: fixture는 Lima의 mkntfs·ntfs-3g(블랙박스)와 시험의 직접 만든 볼륨; Windows가 만든 고정 VHD(런
  384개, 속성 목록)도 맞게 읽었다. 압축·암호화 스트림, 상주 데이터, 시스템 파일은 `rest`.
- **3** ext2/3/4: 1.57 GB 478.3 → 425.7 MB, 31.4 → 53.0 s. 시간의 절반이 마지막 검증이다(아래 "남은 일").
  해제 1.4 s.
- **4** 컨테이너(동적 VHD, VHDX, VMDK 희소, qcow2 v2/v3, VDI): 읽는 쪽이 `Image`(범위로 읽기)를 받는다. 2 GB
  VHDX가 원시 디스크와 같은 크기로 줄었다.
- **5** WOF·LZNT1(Windows가 만든 2 GB VHD, System32): LZX 531.2 → 393.3 MB(-26%), XPRESS4K 696.2 → 379.8
  (-45%), LZNT1 672.8 → 364.2(-46%). aqr-xpress·aqr-lznt1의 기록은 0.35–3%(Windows의 인코더는 탐욕적).
  `lzx-recons` 적용은 32 MiB씩 나누어야 힙이 예산을 지킨다(전체 파일 하나면 3.59 GB).
  검증은 기록이 알리지 않는 값을 스레드에서 미리 평가하고(`eval::hashed`), 블록 캐시는 512 MiB(4개 → 512 MiB:
  LZNT1 VHD 73.8 → 36.3 s).
- **6** APFS: 최신 체크포인트, 객체 맵, B-트리 extent; decmpfs·암호화·스냅숏·xattr는 `rest`. 1.5 GB 346.3 → 325.3 MB.
- **7** HFS+·HFSX와 DMG(UDIF): HFS+ 1.33 GB 245.5 → 231.7 MB. 압축 DMG는 `Format::Dmg`: UDZO 371.6 →
  230.9 MB(hdiutil의 zlib 청크는 zlib 1.2.12 레벨 1과 바이트까지 같아 프로파일 10 에뮬레이션, 기록 0.01%),
  UDBZ(aqr-bz2 `take_whole`) 320.6 → 230.7 MB, ULFO(aqr-bvx `recons::split`) 329.8 → 233.4 MB. 조각난 파일의
  Split은 공식을 만들 때 조각의 해시를 기록해(`Prevalidator::learn_splits`) 검증이 다시 평가하지 않는다
  (UDZO 힙 4.53 → 0.90 GB). 남은 것: 작은 DMG는 모듈 바이트(flate-set 46.7 KB)가 커서 원시로 남는다.
- **8** 압축 컨테이너: qcow2 압축 클러스터(qemu-img: 클러스터마다 zlib 6, 창 4 KiB의 raw DEFLATE; 371.3 →
  231.2 MB), streamOptimized VMDK(zlib 6과 바이트까지 같음; 345.3 → 231.5 MB; OVA는 tar 멤버로 232.4 MB),
  E01(`aqr_disk::ewf`; ewfacquire fast = zlib 1; 379.6 → 231.7 MB, 청크가 작아 레코드가 청크마다 십여 바이트).
  읽지 않는 것: qcow2의 zstd 클러스터(프레임을 다시 만들지 않으므로 얻을 것이 중복 제거뿐; 스캔됨), EWF2(`.Ex01`),
  EnCase 6.7.1의 2 GiB 넘는 32비트 오프셋.

## 남은 일

1. **검증이 이미지 하나를 잎 하나로 본다.** 디스크 이미지 전체에 해당한다. ext4 1.57 GB에서 시간의 절반(26.6 s)이
   검증이다: 기록이 없는 적용 수천 개(`.ko.zst` 5848개 등)를 `eval::hashed`가 하나씩 평가하며 큰 블록을 다시
   푼다. 블록 캐시를 256 MiB로 늘려도 26.6 → 23.6 s뿐이었다. 검증이 잎 안의 값을 블록 순서로 평가하거나,
   큰 이미지를 잎 여럿으로 나누는 쪽을 본다.
2. **해제 힙이 예산을 넘는 경우**(Windows의 3 GB VHD: 3.3 GB). 단일 파일 아카이브는 클래스마다 블록이 128 MiB까지라
   내용물 종류가 많은 이미지에서 동시에 산 블록이 많다. `streaming-encoder-design.md`의 "클래스마다 블록 하나"의 몫.
3. **작은 파일의 묶음**: 하지 않았다. 백만 파일 볼륨에서 공식의 무게를 재고 정한다(문턱 64 KiB 안).
4. **조각과 해제 순서**: 조각난 파일이 많으면 디코더가 스트림을 차례 밖으로 읽는다(spool). 조각화가 심한 NTFS
   (Windows가 오래 쓴 볼륨)에서 디스크 사용과 시간을 아직 재지 않았다. 나쁘면 큰 파일의 조각 순서대로 스트림을 쓴다.
5. **메타데이터**(전부 `rest`, 모듈이 필요한 일; 크기를 재고 큰 것부터): NTFS `$MFT`(파일당 1 KiB, 섹터마다
   fixup; 필드별 분해가 가장 클 것), ext4 inode 표·비트맵·그룹 디스크립터(비트맵과 체크섬은 유도 가능),
   APFS 객체의 Fletcher-64, GPT의 CRC-32.
6. **영 구간**: 빈 공간·sparse 구멍은 `rest`에 넣고 zstd에 맡긴다(크기는 거의 0, 드는 것은 0을 압축·해제·해시하는
   시간). 영 채움 노드(길이만 갖는 노드)는 미뤘다: 평가기의 `Chunk`에 `Zeros(n)`이 생겨야 하고(쓰는 쪽, 내용 해시
   `Ch`의 O(log n), 스트림 병합·zstd만 이용), 스트림은 구멍을 품을 수 없어 내용물 안의 0은 공식을 따로 만들어야
   하며, 공식은 바이트만의 함수여야 하므로 자리는 내용으로(예: 4 KiB 0 블록) 정해야 한다. 얇은 VM 디스크(대부분
   0)에서 0에 드는 시간이 전체에서 큰 몫일 때만 다시 본다(아직 재지 않았다). 입력의 sparse 구멍을 `SEEK_HOLE`
   (Windows `FSCTL_QUERY_ALLOCATED_RANGES`)로 건너뛰는 것도 안 했다(코드에 없음). 0이 아닌 빈 공간은 그대로 둔다.
7. **시험하지 못한 것**: Windows가 만든 동적 VHD·VHDX, VMware·VirtualBox가 내보낸 OVA, EnCase·FTK Imager의 E01
   (표본이 맞지 않으면 기본 프로파일).
8. **다음 형식**: decmpfs(APFS·HFS+; zlib·LZVN·LZFSE: aqr-flate, aqr-bvx), XFS, btrfs, squashfs, EROFS, F2FS,
   Android sparse, 원시 CD 섹터(`other-projects.md` §7): 수요를 보고. squashfs·btrfs의 gzip·xz·lz4·zstd 압축 블록은
   재구성기가 있으므로 파서만 있으면 된다.
9. 선택: 파서가 트리를 읽으므로 `aqr -d`가 디스크 이미지를 외부 아카이브로 풀게 할 수 있다(aqr-unpack은 아직
   디스크 이미지를 모른다). 이름·시각·권한까지 읽어야 하고 재압축과 독립.

## 풀 때의 sparse 출력 [구현]

`crates/aqr/src/sparse.rs`(옵션 `sparse:hole` 256 KiB, `sparse:buffer`): 0을 64 KiB 단위(오프셋도 64 KiB 배수)로
알아보고, 이어지는 0이 256 KiB 이상이면 구멍으로 둔다. 디스크 이미지만이 아니라 모든 파일에 듣는다. 근거가
된 측정(APFS: M4, ext4: Lima, NTFS: 4 KiB 클러스터 NVMe, Defender 검사에서 뺀 폴더):

| | 구멍의 단위 | 구멍 만들기 | 구멍 하나(쓰기와 flush) | 시간의 손익분기 |
|---|---|---|---|---|
| APFS | 4 KiB(16 KiB 미만은 읽기가 나쁘다: 4 KiB 구멍 26만 개 파일 읽기 5.8 s 대 0.1 s) | `F_PUNCHHOLE`만 확실(`lseek`는 처음 약 512개 구멍을 0으로 채운다) | 약 25 µs | 25–75 KB |
| ext4 | 4 KiB, `lseek`로도 정확 | `lseek`, `fallocate(PUNCH_HOLE)` 같다 | 10–55 µs(`fsync`의 몫) | 약 64 KiB |
| NTFS | 64 KiB에 맞춘 64 KiB(작거나 어긋난 구멍은 `FSCTL_SET_ZERO_DATA`로도 그대로 할당) | `FSCTL_SET_SPARSE` 뒤 건너뛰기와 `SET_ZERO_DATA` 같다 | dense 대비 45–55 µs; 실제 구멍이 많은 파일은 순차 읽기도 구멍당 약 40 µs 느리다 | 150–300 KB |

- NTFS에서 구멍이 생기지 않는 경우에도 sparse로 표시하고 건너뛰면 flush가 느려진다(65536개: 3–6 s 대 0.5 s):
  첫 구멍에서 표시한다. 1 MiB 구멍은 세 곳 모두 이득이다.
- 미해결: APFS에서 4 GiB 파일에 16 KiB 구멍 131072개를 뚫으면 쓰기가 28–32 s로 갑자기 나빠진다(65536개, 2 GiB는
  1.5 s). 더러운 데이터의 양과 쓰기 지연의 상호작용으로 보이나 가르지 않았다.

## 명세의 출처

GPL 코드(커널의 파일 시스템 드라이버, ntfs-3g, e2fsprogs, VirtualBox, QEMU의 코드)는 읽지 않는다. 문서와 공개
명세만 읽고, 도구는 fixture를 만드는 블랙박스로만 쓴다(libyal처럼 코드 저장소에 딸린 문서는 문서만).

| 대상 | 출처 |
|---|---|
| FAT, exFAT | fatgen103, Microsoft exFAT 명세 |
| NTFS | 공식 명세 없음: Linux-NTFS 문서, libfsntfs `documentation/` |
| LZNT1, XPRESS | [MS-XCA] |
| WOF | Microsoft 문서의 `WOF_FILE_COMPRESSION_INFO`, 재분석 태그 |
| ext4 | 커널 `Documentation/filesystems/ext4/`(문서만) |
| APFS, HFS+ | Apple File System Reference, TN1150 |
| MBR, GPT, APM | UEFI 명세 5장, Inside Macintosh |
| VHD, VHDX | VHD 명세, [MS-VHDX] |
| VMDK, qcow2 | VMware Virtual Disk Format 1.1, QEMU `docs/interop/qcow2.txt` |
| VDI, E01 | libyal의 문서 |
| DMG(UDIF) | 공개된 해설(`koly` 트레일러, `mish` 표) |

## 시험 자료

- 작은 fixture(수 MB)는 스크립트로 만들어 커밋한다(`codecs/aqr-*/scripts/gen-*.sh`): macOS의 `hdiutil`,
  `newfs_*`; Lima VM(`aqr-linux`)의 `mke2fs`, `mkntfs`, `qemu-img`; Windows 피어의 `format`, `compact`.
  조각난 파일은 작은 파일을 쓰고 지우기를 되풀이해 만든다.
- 측정용 실제 이미지(VM 디스크, 시스템 볼륨, DMG)는 크고 사적인 데이터가 섞이기 쉽다. 사용자가 고르고 재는 것을
  원칙으로 하고, 네트워크 드라이브에서 일괄로 읽지 않는다.

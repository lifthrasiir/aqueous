# 탐색기 안에서 아카이브 열어 보기 (Windows shell namespace extension)

지금 aqr-win은 동사만 있다 (`crates/aqr-win/README.md`): 더블클릭은 "여기에 풀기"이고, 아카이브에
무엇이 들었는지 보려면 `aqr -t`를 써야 한다. 이 문서는 `.aqr` 파일을 탐색기 안에서 폴더처럼 열고,
그 안의 파일을 끌어 내보내고, 더블클릭으로 여는 기능을 어떻게 만드는지 정한다. Windows의 ZIP 폴더
(`zipfldr.dll`)와 같은 자리다.

## 1. 방식의 선택

| 방식 | 형태 | 판단 |
|---|---|---|
| VirtDisk (`AttachVirtualDisk`) | 드라이브 | ISO, VHD, VHDX만 받는다. 내용을 콜백으로 공급할 수 없다 |
| ProjFS | NTFS 폴더 아래 placeholder | 선택 기능이라 켜야 하고, 읽은 파일은 디스크에 hydrate된다 |
| Cloud Files (cfapi) | sync root | sync root 등록과 정리가 무겁고, 동기화 상태 아이콘이 뜬다 |
| WinFsp, Dokany | 드라이브 | 드라이버 설치가 필요하다. 라이선스(GPLv3+예외, LGPL)를 퍼블릭 도메인 배포와 맞춰 봐야 한다 |
| 별도 창 (7-Zip, WinRAR 방식) | 자체 UI | 탐색기와 똑같이 만들 수 없다 |
| **Shell namespace extension** | 탐색기 안의 가상 폴더 | **채택한다** |

namespace extension은 목록 화면을 직접 그리지 않는다. 탐색기의 DefView(`SHCreateShellFolderView`)를
그대로 쓰므로 보기 모드, 정렬, 열, 검색 상자, 주소 표시줄, 탐색 창이 탐색기와 같다. 한계는 파일
시스템이 아니라는 점이다. 경로로 파일을 여는 프로그램을 위해서는 임시 파일로 풀어 줘야 한다(§6.3).
ProjFS 같은 투영 방식은 나중에 "폴더로 마운트" 동사로 덧붙일 수 있다(§9). 그 경우에도 §4의 서버와
캐시는 그대로 쓴다.

## 2. 구성

```text
Explorer 또는 파일 대화상자를 연 아무 프로세스
  └ aqr-shell.dll (in-proc, 얇게)       IShellFolder2, PIDL, IDataObject, 메뉴
        │ COM (CLSCTX_LOCAL_SERVER)
  aqr-win --serve (COM local server)   활성화, 수명, 보안, IStream 프록시
        │ 표준 입출력 (aqr --serve 프로토콜, §4)
  aqr --serve                          아카이브 열기, 디코딩, 캐시
```

- **aqr-shell.dll**(새 crate `aqr-win-shell`, `cdylib`)
  - 공용 파일 대화상자를 여는 모든 프로세스에 로드된다. 그래서 디코더, Wasm 엔진, 큰 캐시는 넣지
    않는다.
  - 크래시가 나면 남의 프로세스가 같이 죽는다. 그러니 panic은 모든 COM 진입점에서
    `catch_unwind`로 잡아 `E_UNEXPECTED`로 바꾼다.
  - 보관하는 상태는 열거 결과와 PIDL 정도다.
- **aqr-win --serve**
  - 이미 `drop.rs`가 쓰는 `LocalServer32` 방식을 따른다.
  - GUI 서브시스템이라 COM이 띄워도 콘솔 창이 뜨지 않는다.
  - aqr-win은 작게(약 1 MB) 유지한다. 디코더는 링크하지 않고 `aqr --serve` 자식 프로세스
    하나(`CREATE_NO_WINDOW`)에 맡긴다.
- **aqr --serve**
  - 디코딩과 캐시는 aqr.exe가 맡는다. 코덱, 네이티브 모듈, 샌드박스(`-Z wasm:`)가 모두 이미 있다.
  - 프로토콜이 표준 입출력이므로 COM과 무관하게 테스트할 수 있고, macOS의 File Provider에서도 다시
    쓸 수 있다.

**aqr.exe를 직접 COM 서버로 두지 않는 이유.** aqr.exe는 콘솔 서브시스템이라 COM이 띄우면 콘솔
창이 생긴다. 또 COM 코드가 CLI 안에 섞인다. 데이터가 한 단계를 더 거쳐 복사되지만 디코딩 비용에
비하면 작다. 측정해 보고 문제가 되면 §8의 공유 메모리로 줄인다.

## 3. 셸 쪽 (aqr-shell.dll)

### 3.1 등록

- 모두 HKCU에 등록한다. 관리자 권한은 필요 없다. `aqr-win --register`가 등록하고 `--install`도
  같은 코드를 쓴다.
- `.aqr`의 ProgID 아래에 `CLSID`를 둔다. `CLSID\{...}\InProcServer32`(`ThreadingModel=Apartment`)와
  `ShellFolder\Attributes`(`SFGAO_FOLDER | SFGAO_BROWSABLE | SFGAO_STREAM` 등)를 둔다. ZIP 폴더의
  `CompressedFolder` 클래스와 같은 방식이다.
- 파일이면서 폴더인 junction으로 동작한다. 셸은 `IPersistFolder3::InitializeEx`(또는
  `IPersistFile::Load`)로 아카이브 경로를 넘겨준다.
- 더블클릭의 의미가 바뀐다. 지금은 "여기에 풀기"(기본 동사)이고, 이제 기본은 열어 보기가 된다.
  "여기에 풀기"와 "검사"는 동사로 남는다. 이 동작을 고를 수 있게 할지는 §10에 열어 둔다.
- 다른 형식(`.zip`, `.7z`, ...)은 처음에는 다루지 않는다(§9). `.zip`은 Windows의 ZIP 폴더와
  부딪친다.

### 3.2 PIDL

아카이브 안 항목 하나가 `SHITEMID` 하나다. 바깥 부분은 아카이브 파일까지의 파일 시스템 PIDL이고,
그 아래가 우리 것이다. PIDL은 즐겨찾기, 최근 항목, 점프 목록에 오래 남으므로 형식을 고정하고 버전을
붙인다.

```text
u16 cb | u32 magic 'AQRI' | u8 version | u8 kind (dir, file, link, other)
| u64 size | i64 mtime (FILETIME) | u32 attributes (Windows' RHSA...)
| u16 name length | name (UTF-16, a component of the path)
```

- 항목은 이름으로만 식별한다. 경로는 PIDL 사슬이 이룬다.
- content index는 PIDL에 넣지 않는다. 아카이브가 `-a`로 바뀌면 index도 바뀌기 때문이다.
- 크기와 시간은 열을 빨리 채우기 위한 사본이다. 서버의 값이 우선한다.
- 이름은 아카이브 안의 바이트를 UTF-16으로 바꾼 것이다. 바꿀 수 없는 이름의 표기는
  `crates/aqr/src/extract.rs`가 풀 때 쓰는 규칙을 그대로 따른다. 그래야 끌어 내보낸 이름과 풀어 쓴
  이름이 같다.

### 3.3 구현할 인터페이스

| 인터페이스 | 하는 일 |
|---|---|
| `IShellFolder2`, `IPersistFolder3`, `IPersistIDList` | 열거(`IEnumIDList`), `BindToObject`(하위 폴더), `CompareIDs`, `GetAttributesOf`, `GetDisplayNameOf`, `ParseDisplayName`(주소 표시줄과 `SHParseDisplayName`), `GetDetailsEx`(열) |
| `CreateViewObject` → `SHCreateShellFolderView` | DefView. `IShellFolderViewCB`로 알림을 받는다 |
| 열(`IShellFolder2::GetDetailsOf`, property key) | 이름, 크기, 수정한 날짜, 종류, 특성(`RHSA`). 압축 후 크기(`aqr -t -v`의 share)는 선택 열로 둔다 |
| 아이콘 | 확장자 기준으로 `SHGetFileInfo(..., SHGFI_USEFILEATTRIBUTES)`를 쓴다. `IExtractIcon`은 셸의 기본 것을 쓴다 |
| `IContextMenu` | `SHCreateDefaultContextMenu`에 콜백을 더한다. 열기, 복사, 여기에 풀기, 속성 |
| `IDataObject` | 끌어 내보내기, 복사(§6.2) |
| `IDropTarget` | 처음에는 없다. 아카이브는 읽기 전용이다. 넣기(`-a`)는 §9 |

- 셸의 COM 인터페이스는 수가 많아서, `drop.rs`처럼 `windows-sys`로 vtable을 손으로 짜기는 어렵다.
  이 crate에는 `windows` crate의 `#[implement]`를 쓴다. 워크스페이스 의존성에 새로 추가해야 한다.
- 대상은 x86-64만이다. ARM64 탐색기는 x64 DLL을 로드하지 못한다. ARM64(EC) 빌드는 나중 일이다.
- 릴리스는 다른 Windows 바이너리와 같이 win7 target으로 빌드한다.

### 3.4 스레드

- 탐색기는 STA에서 호출한다. UI 스레드를 막지 않으려면 열거와 열 값은 서버의 메타데이터만으로
  답해야 한다. 그 값은 빠르다(§5.1).
- DefView가 열거를 백그라운드 스레드에서 하는지는 확인이 필요하다. 아니라면 큰 디렉터리의 첫 열거가
  지연의 대부분이 된다.
- 서버 연결은 프로세스 단위로 하나를 둔다. 아파트가 다른 스레드에 넘길 때는 Global Interface
  Table(`IGlobalInterfaceTable`)로 넘기고, 끊어지면(`RPC_E_DISCONNECTED`, `RPC_S_SERVER_UNAVAILABLE`)
  다시 만든다.

## 4. 서버

### 4.1 aqr-win --serve (COM local server)

- 클래스 `AqueousArchiveServer`를 `LocalServer32 = aqr-win.exe --serve`로 등록하고
  `REGCLS_MULTIPLEUSE`로 노출한다. 단일 인스턴스와 기동 경합은 COM이 처리한다.
- 인터페이스는 하나만 둔다(`IAqrArchives`, 자동화 호환 타입만 써서 표준 마샬링(typelib)을 쓰거나
  proxy/stub을 직접 둔다). 메서드는 다음과 같다.
  - `Open(path) → handle`
  - `List(handle, dir) → entries`
  - `Stat(handle, path)`
  - `OpenStream(handle, path) → IStream`
  - `Materialize(handle, paths) → temp paths`(§6.3)
  - `Close(handle)`
- **수명**
  - `CoAddRefServerProcess`/`CoReleaseServerProcess`로 마지막 클라이언트가 떠난 시점을 안다.
  - 그때 바로 끝내지 않는다. 탐색기는 폴더를 이동할 때마다 객체를 만들었다 놓기 때문이다.
  - 유휴 상태는 두 단계로 나눈다.
    - 1분 뒤: 내용 캐시(§5.2)를 버린다.
    - 10분 뒤: 프로세스를 끝낸다. 이때 `aqr --serve`의 표준 입력을 닫는다.
  - 수치는 정해 둔 것일 뿐이니 측정해 보고 고친다.
- **세션과 사용자**
  - COM의 `LocalServer32` 활성화는 기본적으로 호출한 사용자, 같은 세션의 인스턴스로 이루어진다.
    RDP 세션 여럿이나 사용자 전환이 섞일 때도 따로 뜨는지는 확인한다.
- **무결성 수준**
  - 관리자 권한 프로세스(high IL)의 파일 대화상자에서 부르면 별도 인스턴스가 뜨게 둔다.
  - medium IL 서버에 붙이지 않는다. 그쪽이 단순하고 안전하다.
- 서버가 죽으면 DLL은 다시 만든다. 열려 있던 `IStream`은 실패를 돌려준다. 그러면 복사 대화상자가
  오류를 보여 준다.

### 4.2 aqr --serve (프로토콜)

`--ipc`(`crates/aqr/src/ipc.rs`)처럼 한 줄에 JSON 하나다. 데이터는 따로 framing을 둔다.
여러 요청이 동시에 진행되므로 모든 요청에 `id`를 붙이고, 응답은 순서가 바뀌어 올 수 있다.

```text
> {"hello":1}
< {"hello":1}
> {"id":1,"open":"D:\\x.aqr"}
< {"id":1,"archive":7,"identity":[...],"encrypted":false}
> {"id":2,"list":7,"dir":"a/b"}
< {"id":2,"entries":[{"name":"c.txt","kind":"file","size":123,"mtime":...,"attrs":...},...]}
> {"id":3,"read":7,"path":"a/b/c.txt","offset":0,"len":1048576}
< {"id":3,"data":1048576}            그 뒤에 1048576바이트가 그대로 온다
> {"id":4,"close":7}
```

- `read`는 범위로 읽는다. `IStream::Read`, `Seek`와 그대로 대응한다.
- 첫 `read`에서 그 content가 속한 leaf를 디코딩한다(§5.2). 그 뒤의 `read`는 캐시에서 답한다.
- 데이터는 표준 출력 하나에 섞여 간다. 그래서 청크 상한(1 MiB)을 두어, 큰 읽기 하나가 다른 요청의
  응답을 오래 막지 않게 한다.
- 진행률: 큰 leaf를 디코딩하는 동안 `{"id":3,"progress":[done,total]}`를 보낸다. 셸 쪽은 이를 복사
  대화상자의 진행률로 쓸 수 있다(§6.2).
- 암호(§7): `{"id":1,"ask":"passphrase",...}` 질문이 오면 셸 쪽이 답한다.
  `--ipc`의 `ask`/`answer`와 같은 모양이다.

## 5. 캐시

셸은 파일 내용을 캐시해 주지 않는다. 아이콘은 시스템 이미지 목록에, 썸네일(`IThumbnailProvider`를
둘 때)은 thumbcache에 캐시된다. 비파일시스템 항목의 썸네일도 캐시되는지, 그러려면 무엇을 줘야
하는지는 확인한다. 그 밖은 모두 `aqr --serve`의 몫이다.

### 5.1 열린 아카이브

- 키는 아카이브 파일의 정체(볼륨 일련번호 + 파일 ID)와 크기, 마지막 수정 시각이다.
  `open`할 때마다 이 셋을 다시 확인한다. 달라졌으면 새로 연다. `-a`나 다른 프로그램이 파일을
  바꾼 경우다.
- 바뀐 것을 보면 셸 쪽에 알리고 DLL은 `SHChangeNotify(SHCNE_UPDATEDIR)`를 보낸다.
  `ReadDirectoryChangesW`로 미리 감시할지는 나중에 정한다.
- 가진 것은 파일의 `Mmap`(`decompress.rs`의 `map_archive`)과 그 위의
  `aqr_archive::Archive`(`open_indexed` 계열: 메타데이터 블록만 읽는다)다.
  - `Archive<'c, 'n>`이 mmap을 빌리므로 자기 참조 구조가 된다.
  - 서버 수명 동안 mmap을 leak해서 `'static`으로 만들고, 닫을 때 직접 푸는 것이 가장 단순하다.
- 열린 동안에는 mmap 때문에 파일이 잠긴다. 공유 모드는 읽기와 삭제 허용이지만, 매핑된 파일은
  잘라내기와 덮어쓰기가 실패한다. 그래서 이렇게 한다.
  - 보기가 닫히면(§4.1의 1단계) mmap을 놓는다.
  - `-a`가 열린 아카이브에 쓰지 못하면 그 오류가 그대로 보인다.
  - 이 상황은 aqr-win의 "이름 붙이기" 동사에서도 마주친다. 그래서 mmap 대신 `ReadFile`로 읽을지는
    §10에 남긴다.
- 디렉터리 목록은 트리에서 바로 만든다. 큰 아카이브라면 디렉터리별 색인을 한 번 만들어 둔다.

### 5.2 내용

- 단위는 leaf다. `Archive::contents_of`는 원하는 content가 든 leaf만 평가하고, 그 leaf가 slice하는
  non-leaf 블록만 디코딩한다.
- solid block이라 한 leaf에 파일이 여럿 들어 있다. 그러니 원한 것만 남기지 말고 그 leaf의 출력을
  모두 캐시한다. 탐색기는 대개 같은 폴더의 파일을 이어서 읽는다.
- LRU는 바이트로 상한을 둔다. 기본값은 물리 메모리의 1/8, 1 GiB 이하로 하고, 측정해 보고 고친다.
  키는 (아카이브, leaf)다.
- 같은 leaf를 동시에 요청하면 한 번만 디코딩하고 나머지는 기다리게 한다.
- **캐시 상한보다 큰 content**(수 GB 파일 하나)는 메모리에 둘 수 없다.
  - 지금 트리 아카이브에서 content를 sink로 흘려 쓰는 API가 없다. `write_file`은 단일 파일
    아카이브만 받고, `contents_of`는 `Vec<u8>`을 통째로 준다.
  - 그래서 처음에는 그런 content를 임시 파일로 spill해서 거기서 읽는다.
  - 근본 해결은 aqr-archive에 트리 아카이브의 content를 흘려 쓰는 경로를 두는 것이다.
    `write_file`이 쓰는 `eval::run` 방식이다.
- **leaf 하나의 비용.** 작은 파일 하나를 꺼내는데 큰 solid leaf 전체를 풀어야 할 수 있다.
  CLAUDE.md에 남은 과제로 적힌 "top-level 엔트리 의존 closure의 바이트 상한"이 여기서 체감 지연으로
  드러난다. 이 기능이 그 과제의 첫 소비자가 된다.
- 디코딩 스레드 수: 대화형 작업이므로 첫 바이트까지의 시간이 중요하다. `aqr -d`와 같은 pool을
  쓰되, 진행 중인 요청이 여럿이면 나눠 쓴다.

### 5.3 임시 파일

§6.3이 쓴다. 위치는 `%LOCALAPPDATA%\Aqueous\Open\<아카이브 정체의 해시>\<아카이브 안 경로>`다.

- 읽기 전용 특성을 붙인다. ZIP 폴더처럼 사용자가 임시 사본을 고쳐 저장했다가 잃는 일을 막는다.
  저장하려 하면 프로그램이 "다른 이름으로 저장"을 묻게 된다.
- 정리 시점은 세 가지다.
  - 서버가 끝날 때: 열려 있지 않은 것을 지운다. 지우지 못하는 것(열려 있는 것)은 남긴다.
  - 서버가 시작할 때: 하루 지난 것을 지운다.
  - 아카이브가 바뀌어 정체가 달라졌을 때: 이전 것을 지운다.
- 이미 풀어 둔 것이 있고 정체가 같으면 다시 풀지 않는다.

## 6. 동작

### 6.1 열어 보기

- 열거하면 `list`를 보낸다. 열 값은 응답의 메타데이터로 채운다.
- 하위 폴더로 들어가면(`BindToObject`) 서버의 같은 handle을 쓴다.
- 항목 수가 많은 디렉터리는 응답을 나눠 받는다. DefView의 열거가 점진적이기 때문이다.

### 6.2 끌어 내보내기, 복사

- `IDataObject`에 가상 파일을 싣는다. `CFSTR_FILEDESCRIPTORW`에 이름, 크기, 시간, 특성을 넣고
  하위 폴더는 상대 경로 항목으로 넣는다. `CFSTR_FILECONTENTS`는 index마다 `IStream`을 준다.
- `IStream`은 서버의 것을 표준 마샬링한 프록시다. `Read` 하나가 RPC 왕복 하나다. 탐색기의 복사는
  큰 버퍼로 읽으니 괜찮다.
- `IDataObjectAsyncCapability`를 구현한다. 그래야 탐색기가 복사를 백그라운드 스레드에서 하고 진행률
  대화상자를 띄운다. 큰 leaf를 디코딩하는 동안 UI가 멈추지 않는 것이 이 기능의 핵심이다.
- 디코딩이 실패하면(손상, 암호 포기) `Read`가 실패를 돌려준다. 그러면 탐색기가 그 파일의 오류를
  보여 준다.
- `CF_HDROP`(실제 경로)은 처음에는 주지 않는다. 경로만 받는 대상(오래된 프로그램)을 위해서는
  §6.3처럼 풀어서 줄 수 있지만, 그러면 끄는 동안 디코딩이 일어난다.

### 6.3 열기 (더블클릭)

- 서버에 `Materialize`를 요청해 임시 파일(§5.3)로 풀고, 그 경로로 `ShellExecuteEx`한다.
- 디코딩이 0.4초를 넘기면 진행 창을 띄운다(aqr-win의 대화상자와 같은 규칙).
- 같은 폴더의 다른 파일을 함께 쓰는 프로그램이 있다. 예를 들어 `.cue`와 그 `.bin`, HTML과 그
  이미지들이다. 그래서 같은 폴더의 형제도 같이 풀지 고를 수 있게 한다. 처음에는 파일 하나만 푼다.
  ZIP 폴더는 형제를 풀지 않는다.

### 6.4 파일 대화상자

- 신형 `IFileDialog`는 우리 폴더를 탐색할 수 있다.
- 열기 대화상자에서 고르면 대화상자는 셸 항목을 돌려준다. `FOS_FORCEFILESYSTEM`인 프로그램은
  고를 수 없다.
- 셸 항목을 받은 프로그램은 `IShellItem::BindToHandler(BHID_Stream)`으로 읽는다. 그러니 우리 항목에
  `BindToObject(IID_IStream)`을 구현해서 서버의 스트림을 준다.

## 7. 암호와 손상

- **암호화된 아카이브**
  - 서버가 `ask:passphrase`를 보내면 DLL이 암호 대화상자를 띄운다. 부모 창은 `IShellFolder`가 받은
    hwnd나 사이트에서 얻는다.
  - 대화상자는 aqr-win의 `passphrase.rs`와 같은 것이다. 코드는 aqr-l10n 문구와 함께 이 crate로
    옮기거나 공유한다.
  - 메타데이터까지 암호화된 아카이브는 열거 전에 묻는다.
  - 서버는 그 아카이브를 닫을 때까지 키를 메모리에 둔다. 서버가 끝나면 지운다(zeroize).
- **손상된 아카이브**
  - 메타데이터를 읽지 못하면 열거가 실패한다. 탐색기는 HRESULT의 일반 오류를 보여 준다.
  - `aqr`의 메시지를 보이려면 DefView 위에 직접 오류 대화상자를 띄운다.
  - salvage(`open_salvaging`)로 읽은 부분만 보일지는 §10에 남긴다.

## 8. 성능 목표와 측정

- 열거: 1만 항목 디렉터리의 첫 표시가 0.5초 이하.
- 끌어 내보내기: 처리량이 `aqr -d`로 같은 파일을 푸는 것의 90% 이상. 넘지 못하면 표준 출력 대신
  공유 메모리(파일 매핑)로 데이터를 넘긴다.
- 메모리: 서버의 상주량을 `--stats`처럼 기록한다. 유휴 1단계 뒤에는 메타데이터만 남아야 한다.
- 10gb 같은 큰 아카이브의 첫 열기 시간, 메타데이터 블록을 읽는 시간을 함께 잰다.

## 9. 단계

1. **`aqr --serve`**: 프로토콜, 열린 아카이브, leaf 캐시, spill.
   - 플랫폼과 무관하게 aqr의 테스트로 검증한다. 목록이 `aqr -t --json`과 같고, `read`한 바이트가
     `aqr -d`로 푼 것과 같은지 본다.
2. **aqr-win --serve**: COM 클래스, 수명, `IStream` 프록시, 재연결.
3. **aqr-shell.dll 열어 보기**: 등록, PIDL, `IShellFolder2`, DefView, 열, 아이콘.
   - PowerShell의 `Shell.Application`(`NameSpace(...).Items()`)으로 자동 테스트한다. 동사 테스트의
     하니스와 같은 방식이다.
4. **끌어 내보내기, 복사**: `IDataObject`, async.
5. **열기**: 임시 파일, 진행 창, 컨텍스트 메뉴.
6. 암호화된 아카이브, 손상의 표시.
7. 그 뒤:
   - 썸네일과 미리 보기(`IThumbnailProvider`, `IPreviewHandler`: 아카이브 안 그림)
   - 속성 시트
   - 판(journal의 지점)을 하위 폴더로 보이기
   - 다른 형식(aqr-unpack이 읽는 것)
   - 넣기(`IDropTarget` → `aqr -a`)
   - ProjFS로 "폴더로 마운트"
   - Windows 11의 새 컨텍스트 메뉴(README의 Status)

## 10. 열린 질문

- 더블클릭의 기본: 열어 보기로 할지, 지금처럼 "여기에 풀기"로 둘지, 설정으로 둘지.
- mmap과 `ReadFile`: 열린 아카이브를 잠그지 않으려면 `ReadFile`이 낫다. 그러면 `Archive`가 `&[u8]`
  전체를 받는 지금의 API를 어떻게 할지 정해야 한다.
- 손상된 아카이브에서 salvage한 부분을 보일지.
- 서버 하나가 모든 아카이브를 맡을지, 아카이브마다 `aqr --serve`를 따로 둘지. 따로 두면 한
  아카이브의 크래시가 다른 것에 번지지 않는다. 대신 캐시 상한을 나눠야 한다.

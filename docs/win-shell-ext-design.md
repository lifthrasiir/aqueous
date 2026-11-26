# 탐색기 안에서 아카이브 열어 보기 (Windows shell namespace extension)

`.aqr` 파일을 탐색기 안에서 폴더처럼 열고, 그 안의 파일을 끌어 내보내고, 더블클릭으로 여는 기능의
설계와 그 구현 상태다. Windows의 ZIP 폴더(`zipfldr.dll`)와 같은 자리다. 구현은 세 crate에 나뉜다:
`crates/aqr/src/serve.rs`(`aqr --serve`), `crates/aqr-win/src/serve.rs`(`aqr-win --serve`),
`crates/aqr-win-shell`(`aqr-shell.dll`). 세부는 각 파일의 모듈 문서와 aqr-win-shell의 README에 있고,
이 문서는 결정과 그 이유를 적는다.

## 1. 방식의 선택

| 방식 | 형태 | 판단 |
|---|---|---|
| VirtDisk (`AttachVirtualDisk`) | 드라이브 | ISO, VHD, VHDX만 받는다. 내용을 콜백으로 공급할 수 없다 |
| ProjFS | NTFS 폴더 아래 placeholder | 선택 기능이라 켜야 하고, 읽은 파일은 디스크에 hydrate된다 |
| Cloud Files (cfapi) | sync root | sync root 등록과 정리가 무겁고, 동기화 상태 아이콘이 뜬다 |
| WinFsp, Dokany | 드라이브 | 드라이버 설치가 필요하다. 라이선스(GPLv3+예외, LGPL)를 퍼블릭 도메인 배포와 맞춰 봐야 한다 |
| 별도 창 (7-Zip, WinRAR 방식) | 자체 UI | 탐색기와 똑같이 만들 수 없다 |
| **Shell namespace extension** | 탐색기 안의 가상 폴더 | **채택했다** |

namespace extension은 목록 화면을 직접 그리지 않는다. 탐색기의 DefView(`SHCreateShellFolderView`)를
그대로 쓰므로 보기 모드, 정렬, 열, 검색 상자, 주소 표시줄, 탐색 창이 탐색기와 같다. 한계는 파일
시스템이 아니라는 점이다. 경로로 파일을 여는 프로그램을 위해서는 임시 파일로 풀어 준다(§6.3).
ProjFS 같은 투영 방식은 나중에 "폴더로 마운트" 동사로 덧붙일 수 있다(§9). 그 경우에도 §4의 서버와
캐시는 그대로 쓴다.

## 2. 구성

```text
Explorer 또는 파일 대화상자를 연 아무 프로세스
  └ aqr-shell.dll (in-proc, 얇게)       IShellFolder2, PIDL, IDataObject, 메뉴, 열, 썸네일
        │ COM (CLSCTX_LOCAL_SERVER, IDispatch)
  aqr-win --serve (COM local server)   활성화, 수명, 암호 대화상자, IStream, Materialize
        │ 표준 입출력 (aqr --serve 프로토콜, §4.2)
  aqr --serve                          아카이브 열기, 디코딩, 캐시
```

- **aqr-shell.dll**(crate `aqr-win-shell`, `cdylib`)
  - 공용 파일 대화상자를 여는 모든 프로세스에 로드된다. 그래서 디코더, Wasm 엔진, 큰 캐시는 넣지
    않는다. 보관하는 상태는 PIDL, 열거한 항목(열 값을 위해), 서버 객체뿐이다.
  - 크래시가 나면 남의 프로세스가 같이 죽는다. 그러니 panic은 모든 COM 진입점에서
    `catch_unwind`로 잡아 실패로 바꾼다.
  - cargo는 라이브러리 이름에 `-`를 못 쓰므로 빌드 결과는 `aqr_shell.dll`이다. 배포하고 등록하는
    이름은 `aqr-shell.dll`이다(다른 바이너리와 맞춤). `cargo aqr-win`(bincache)과 설치 프로그램이
    복사할 때 이름을 바꾼다.
- **aqr-win --serve**
  - `drop.rs`가 쓰는 `LocalServer32` 방식을 따른다. GUI 서브시스템이라 COM이 띄워도 콘솔 창이 뜨지
    않는다.
  - aqr-win은 작게 유지한다. 디코더는 링크하지 않고 `aqr --serve` 자식 프로세스 하나
    (`CREATE_NO_WINDOW`)에 맡긴다. 모든 아카이브가 이 하나를 같이 쓴다(§10).
- **aqr --serve**
  - 디코딩과 캐시는 aqr.exe가 맡는다. 코덱, 네이티브 모듈, 샌드박스(`-Z wasm:`)가 모두 이미 있다.
  - 프로토콜이 표준 입출력이므로 COM과 무관하게 테스트할 수 있고(`crates/aqr/tests/serve.rs`),
    macOS의 File Provider에서도 다시 쓸 수 있다. 프론트엔드 쪽 구현은 `aqr_ipc::serve`다.

**aqr.exe를 직접 COM 서버로 두지 않는 이유.** aqr.exe는 콘솔 서브시스템이라 COM이 띄우면 콘솔
창이 생긴다. 또 COM 코드가 CLI 안에 섞인다. 데이터가 한 단계를 더 거쳐 복사되지만 디코딩 비용에
비하면 작다. 측정해 보고 문제가 되면 §8의 공유 메모리로 줄인다.

## 3. 셸 쪽 (aqr-shell.dll)

### 3.1 등록

- 모두 HKCU에 등록한다. 관리자 권한은 필요 없다. `aqr-win --register`(설치 프로그램은
  `--install`)가 `aqr-shell.dll`이 aqr-win.exe 옆에 있을 때만 등록하고, 없으면 지운다.
- 폴더 클래스 `CLSID\{f76e4146-...}`: `InProcServer32`(`ThreadingModel=Apartment`),
  `ShellFolder\Attributes`(`SFGAO_FOLDER | SFGAO_STREAM`), 그리고
  `Implemented Categories\{00021490-...}`(CATID_BrowsableShellExt). 마지막 것이 없으면 셸이 파일을
  junction으로 받지 않는다(ZIP 폴더도 이것을 둔다).
- `.aqr`의 ProgID(`Aqueous.Archive`) 아래에 `CLSID`를 둔다. 셸은 `IPersistFolder3::InitializeEx`
  (또는 `IPersistFolder::Initialize`)로 아카이브 파일의 ID list를 넘겨준다.
- **더블클릭은 열어 보기다**(§10에서 결정). ProgID의 기본 동사를 `open`으로 하고, 그 command에
  `DelegateExecute = {11dbb47c-...}`(CLSID_ExecuteFolder)를 둔다. "여기에 풀기"와 "검사"는 동사로
  남는다.
- 속성의 "버전" 탭 클래스 `CLSID\{54394c15-...}`(`InProcServer32`, 같은 DLL)와 ProgID의
  `shellex\PropertySheetHandlers\Aqueous`(§6.8).
- 서버 클래스 `CLSID\{c6b7e992-...}`의 `LocalServer32 = "aqr-win.exe" --serve`.
- 다른 형식(`.zip`, `.7z`, ...)은 다루지 않는다(§9). `.zip`은 Windows의 ZIP 폴더와 부딪친다.

### 3.2 PIDL

아카이브 안 항목 하나가 `SHITEMID` 하나다(`src/pidl.rs`). 바깥 부분은 아카이브 파일까지의 파일
시스템 PIDL이고, 그 아래가 우리 것이다. PIDL은 즐겨찾기, 최근 항목, 점프 목록에 오래 남으므로 형식을
고정하고 버전을 붙였다.

```text
u16 cb | u32 magic 'AQRI' | u8 version (1) | u8 kind (dir, file, link, other, journal point)
| u64 size | i64 mtime (FILETIME) | u32 attributes (Windows')
| u16 name length | name (UTF-16, a component of the path)
```

- 항목은 이름으로만 식별한다. 경로는 PIDL 사슬이 이룬다. content index는 넣지 않는다. 아카이브가
  `-a`로 바뀌면 index도 바뀌기 때문이다.
- 크기와 시간은 열을 빨리 채우기 위한 사본이다.
- 이름은 `aqr -d`가 풀 때 쓰는 이름 그대로다(`crates/aqr/src/names.rs`, 서버가 만들어 준다). 그래서
  끌어 내보낸 이름과 풀어 쓴 이름이 같다.
- journal의 지점(판, kind 4)은 아카이브 파일 바로 다음에만 오는 항목이다. 그 뒤의 항목은 그 지점의
  아카이브의 것이다. size는 지점을 가리키는 오프셋(서버의 `metadata`), mtime은 지점의 시각, 이름은
  보이는 이름("라벨 (시각)")이다. 목록에는 나오지 않고 메뉴로만 간다(§6.8). 파싱 이름은 `@`와
  오프셋이다(`x.aqr\@1234\a.txt`). 루트에 같은 이름의 항목이 있으면 항목이 먼저다.

### 3.3 구현한 인터페이스

| 인터페이스 | 하는 일 |
|---|---|
| `IShellFolder2`, `IPersistFolder3`, `IPersistIDList` | 열거, `BindToObject`(하위 폴더, 파일의 `IStream`), `CompareIDs`, `GetAttributesOf`, `GetDisplayNameOf`, `ParseDisplayName`(없으면 `ERROR_FILE_NOT_FOUND`), `GetDetailsEx`/`GetDetailsOf`(열) |
| `CreateViewObject` → `SHCreateShellFolderView` | DefView |
| 열 (`src/columns.rs`) | `aqr -t -v`가 말하는 모든 것(formula만 빼고): 이름, 수정한 날짜, 유형, 크기가 기본이고, 데이터, 부가 블록, 비율, 세그먼트, 소유자, 권한, 특성(`RHSATI`), macOS 플래그, 플래그, 만든 날짜, 액세스한 날짜, 메타데이터 바꾼 날짜, 설명, 링크 대상은 "자세히…"에서 고른다. 시스템 속성이 있는 것은 그 키를 써서 셸이 사용자 언어로 이름 붙이고 보여 주고, 나머지는 우리 속성(FMTID `d973fccd-...`)으로 aqr-l10n이 이름 붙인다 |
| 아이콘 | 확장자 기준 `SHCreateFileExtractIconW` |
| `IContextMenu` (`src/menu.rs`) | `SHCreateDefaultContextMenu`에 콜백을 더한다. 복사, 속성, 폴더의 동사는 셸 것이고, 파일 하나를 고르면 "열기"(기본)와 "옆의 파일과 함께 열기"(§6.3)를 앞에 넣는다 |
| `IDataObject` (`src/data.rs`) | 셸의 data object(`SHCreateDataObject`, 항목들의 ID list). 안쪽 객체가 다른 프로그램용으로 `CFSTR_FILEDESCRIPTORW`, `CFSTR_FILECONTENTS`를 준다(§6.2) |
| `IQueryAssociations`, `IThumbnailProvider`, `IExtractImage2` (`src/handlers.rs`) | 파일 형식의 연결과 썸네일(§6.6) |
| 속성 시트 (`src/props.rs`) | 메뉴의 속성, Alt+Enter(§6.7) |
| `IDropTarget` | 없다. 아카이브는 읽기 전용이다. 넣기(`-a`)는 §9 |

- 셸의 COM 인터페이스는 수가 많아서 `windows` crate의 `#[implement]`를 쓴다(워크스페이스 의존성).
  aqr-win 쪽은 객체가 셋뿐이라 `windows-sys`로 vtable을 손으로 짰다.
- 대상은 x86-64만이다. ARM64 탐색기는 x64 DLL을 로드하지 못한다. ARM64(EC) 빌드는 나중 일이다.
- 릴리스는 다른 Windows 바이너리와 같이 win7 target으로 빌드한다. 쓰는 셸 API는 모두 Vista 이후
  것이다. 다만 `windows` crate가 Windows 8 이후의 DLL을 링크한다(windows-core의 `combase.dll`:
  `CoTaskMemAlloc`, `CoTaskMemFree`, `CoCreateFreeThreadedMarshaler`; windows-result의
  `api-ms-win-core-winrt-error-l1-1-0.dll`: `RoOriginateErrorW`). 그대로면 Windows 7에서 DLL이
  로드되지 않는다. 그래서 둘을 delay-load하고(`build.rs`), hook(`src/delay.rs`)이 `combase.dll`이
  없으면 `ole32.dll`을, `RoOriginateErrorW`가 없으면 아무것도 하지 않는 함수를 준다.

### 3.4 스레드와 아파트

- 탐색기는 창마다 STA 스레드를 두고, DefView는 열거와 썸네일을 백그라운드 스레드에서도 부른다.
- **서버 객체는 프로세스의 MTA가 가진다**(`src/client.rs`). 처음에는 Global Interface Table에
  넣었는데, 쿠키가 등록한 아파트(처음 연 창의 스레드)에 속해서, 그 창을 닫으면 항목이 사라지고 다른
  곳에서 같은 아카이브를 다시 쓸 때 `E_INVALIDARG`가 났다. 그래서 DLL의 스레드 하나가 MTA를 프로세스
  수명 동안 붙들고(그 동안 DLL은 pin해서 내려가지 않는다), 서버 호출은 언제나 MTA의 스레드에서 한다.
  호출자 아파트로 넘겨야 하는 스트림만 GIT로 그 자리에서 건넨다.
- 잠금을 잡은 채 COM 호출을 하지 않는다. STA의 바깥 호출은 메시지를 돌리므로 같은 스레드로 다시
  들어올 수 있다.
- 서버가 없어지면(`RPC_E_DISCONNECTED` 등) 가진 것을 버리고 한 번 다시 시도한다.
  `DISP_E_UNKNOWNNAME`(모르는 메서드: 이전 빌드의 서버)도 같이 다룬다.

## 4. 서버

### 4.1 aqr-win --serve (COM local server)

- 클래스는 `REGCLS_MULTIPLEUSE`로 노출한다. 단일 인스턴스와 기동 경합은 COM이 처리한다.
- **타입 라이브러리 없는 `IDispatch`**다. 시스템의 프록시가 마샬링하므로 proxy/stub DLL이나 typelib
  등록이 필요 없다. 메서드(인자는 문자열, 결과는 JSON 문자열이나 객체):
  - 클래스: `Open(path[, hwnd[, at]])` → 아카이브 객체(`at`: journal 지점의 오프셋. 그 지점의
    아카이브)
  - 아카이브: `Info()`, `List(dir[, "verbose"])`, `Stat(path[, "verbose"])`, `OpenStream(path)` →
    `IStream`, `Materialize(path)`, `Progress(path)`, `Cancel(path)`, `Journal()`
- 아카이브 객체가 놓이면(클라이언트가 놓거나, 클라이언트가 죽어 COM이 정리하면) 그 handle은 1분 뒤에
  닫는다. 그 사이 다시 열면 그대로 쓴다. 탐색기가 폴더를 옮길 때마다 객체를 만들었다 놓기 때문이다.
- 객체가 하나도 없이 10분이 지나면 클래스를 revoke하고 끝난다. 수치는 정해 둔 것이니 재 보고 고친다.
- 2초마다 레지스트리의 `LocalServer32`가 자기 실행 파일을 가리키는지 본다. 다른 빌드로 다시
  등록됐거나(업데이트, `cargo aqr-win --register`), 등록이 지워졌으면(제거) 클래스를 revoke하고 객체가
  없어지는 즉시 끝난다. 이전 빌드의 서버가 새 DLL을 계속 받는 일을 막는다.
- 세션의 이벤트 `Local\Aqueous.Serve.Quit`이 켜지면 곧바로 끝난다. 제거가 파일을 지우기 전에 쓴다
  (§12).
- 서버가 죽으면 DLL은 다시 만든다. 열려 있던 `IStream`은 실패를 돌려주고, 복사 대화상자가 오류를
  보여 준다.
- 확인하지 않은 것: RDP 세션 여럿이나 사용자 전환에서 따로 뜨는지, 관리자 권한(high IL) 프로세스의
  파일 대화상자에서 부를 때 별도 인스턴스가 뜨는지.

### 4.2 aqr --serve (프로토콜)

`--ipc`처럼 한 줄에 JSON 하나다. 여러 요청이 동시에 진행되므로 모든 요청에 `id`를 붙이고, 응답은
순서가 바뀌어 올 수 있다. 전체는 `crates/aqr/src/serve.rs`의 모듈 문서에 있다.

```text
> {"hello":1}
< {"hello":1}
> {"id":1,"open":"D:\\x.aqr"}
< {"id":1,"archive":7,"identity":"...","single":false}
> {"id":2,"list":7,"dir":"a/b"}
< {"id":2,"entries":[{"name":"c.txt","kind":"file","size":123,"mtime":[1700000000,0],...}],"more":false}
> {"id":3,"read":7,"path":"a/b/c.txt","offset":0,"len":1048576}
< {"id":3,"data":1048576}            그 뒤에 1048576바이트가 그대로 온다
> {"id":4,"close":7}
```

- `open`: 같은 파일(경로와 identity)을 다시 열면 같은 handle이다. 암호화된 아카이브는
  `passphrase`를 같이 받는다. 없으면 `encrypted`, 틀리면 `wrong`이 붙은 오류다.
- `list`, `stat`: 항목은 `aqr -t -v`가 보여 주는 것을 다 담는다(`owner`, `mode`, `win`, `mac`,
  `flags`, 시각들, `comment`, `target`). `verbose`면 `data`, `overhead`, `share`, `ratio`,
  `segment`, `formula`, `nodes`도 담는다. 이것들은 아카이브의 블록을 다 읽어야 해서 처음 한 번
  만들고 둔다. 디렉터리는 4096개씩 나눠 보낸다(`more`).
- `read`: 범위로 읽고, 한 번에 1 MiB까지다. 큰 읽기 하나가 다른 요청의 응답을 오래 막지 않게 한다.
- `progress`: 아카이브가 마지막으로 알려 준 진행(`decompressed`, `evaluated`)과 평가 중인 leaf 수.
  요청별로 나뉘지 않는다(§10).
- `journal`: journal의 지점들(판). 순서대로 `metadata`(지점을 가리키는 오프셋), `time`, `shown`(aqr가
  보여 주는 지역 시각), `label`. `open`에 `at`(지점의 `metadata`)을 주면 그 지점의 아카이브를 연다
  (`--at`과 같다). handle은 따로다. 그 지점의 아카이브는 그 뒤의 지점을 모른다.
- `trim`: 둔 내용을 버린다. `close`: handle을 놓는다.

## 5. 캐시

### 5.1 열린 아카이브

- 키는 아카이브 파일의 정체(볼륨 일련번호 + 파일 ID)와 크기, 마지막 수정 시각이다.
- 가진 것은 파일의 mmap과 그 위의 `Archive`(`open_indexed`: 메타데이터 블록만 읽는다), 암호화된
  것은 복호화 중인 payload다. `Archive`가 그것들을 빌리므로, 빌린 것들을 따로 leak해 두고 필드
  순서대로 아카이브 다음에 놓는다(`Leaked`).
- 열린 동안에는 mmap 때문에 파일이 잠긴다. 매핑된 파일은 잘라내기와 덮어쓰기가 실패한다. 보기가 다
  닫히면 1분 뒤 handle과 함께 mmap을 놓는다. 그 전에 `-a`가 쓰지 못하면 그 오류가 그대로 보인다.
  `ReadFile`로 바꿀지는 §10에 남긴다.

### 5.2 내용

- 단위는 leaf다. 원하는 content가 든 leaf만 평가하고(`each_leaf_only`), 그 leaf의 출력을 모두
  둔다. 탐색기는 대개 같은 폴더의 파일을 이어서 읽는다.
- LRU는 바이트로 상한을 둔다. 기본값은 물리 메모리의 1/8, 1 GiB 이하(`-Z serve:cache`)다. 키는
  (아카이브, leaf)다. 같은 leaf를 동시에 요청하면 한 번만 평가하고 나머지는 기다린다.
- 상한보다 큰 출력은 임시 파일(닫으면 지워지는)로 spill해서 거기서 읽는다. 다만 평가하는 동안에는
  leaf 전체가 메모리에 한 번 올라온다. 근본 해결은 aqr-archive에 트리 아카이브의 content를 흘려 쓰는
  경로를 두는 것이다(`write_file`이 쓰는 `eval::run` 방식). 단일 파일 아카이브는 이미 흘려 쓴다.
- **leaf 하나의 비용.** 작은 파일 하나를 꺼내는데 큰 solid leaf 전체를 풀어야 할 수 있다.
  CLAUDE.md에 남은 과제로 적힌 "top-level 엔트리 의존 closure의 바이트 상한"이 여기서 체감 지연으로
  드러난다.

### 5.3 임시 파일

`Materialize`(§6.3)가 `%LOCALAPPDATA%\Aqueous\Open\<아카이브 경로와 identity의 해시>\<아카이브 안 경로>`에
쓴다.

- `.part`에 쓰고 제자리로 이름을 바꾼다. 수정 시각은 항목의 것이고, 읽기 전용 특성을 붙인다. 사용자가
  임시 사본을 고쳐 저장했다가 잃는 일을 막는다.
- 이미 같은 크기의 파일이 있으면 다시 쓰지 않는다.
- 서버가 끝날 때 지운다(열려 있는 것은 남는다). 서버가 시작할 때 하루 지난 것을 지운다.

## 6. 동작

### 6.1 열어 보기

- 열거하면 `list`를 보내고, 받은 항목을 폴더 객체가 이름별로 둔다. 열 값은 PIDL이나 둔 항목에서
  채워서 칸마다 서버를 부르지 않는다. 데이터, 비율 같은 열이 처음 필요하면 그 폴더를 `verbose`로 한
  번 더 받는다.

### 6.2 끌어 내보내기, 복사

- 셸의 data object(`SHCreateDataObject`, 항목들의 ID list)를 준다. Windows 8 이후 탐색기의 복사
  엔진은 그것을 폴더와 스트림(`BHID_Stream`)으로 읽으므로, 붙여 넣기와 탐색기 폴더로의 끌기는
  이것만으로 된다. 복사 대화상자와 진행도 복사 엔진의 것이다.
- **Windows 7**의 복사 엔진과 폴더의 drop target은 ID list의 항목을, 원본 폴더가
  `ITransferSource`(`CreateViewObject`)를 줄 때만 받는다. 그래서 Windows 7에서는 안쪽 객체(descriptor와
  contents)만 준다. drop target은 그것을 Outlook의 첨부처럼 받아 스스로 복사한다(VM에서 확인했다).
  `ITransferSource`를 구현하면 Windows 7에서도 복사 엔진의 진행 창을 쓸 수 있다(§10).
- 셸이 아닌 프로그램(브라우저의 업로드, 메일의 첨부)으로 끌 때는 안쪽 객체가
  `CFSTR_FILEDESCRIPTORW`(고른 것과 그 아래 폴더의 모든 항목, 폴더로부터의 경로)와
  `CFSTR_FILECONTENTS`(index마다 서버의 `IStream`)를 준다.
- `IStream`은 서버 것의 프록시다. `Read` 하나가 RPC 왕복 하나이고, 한 번에 1 MiB씩 서버에서 받는다.
- `CF_HDROP`(실제 경로)은 주지 않는다.

### 6.3 열기 (파일의 더블클릭)

- 서버에 `Materialize`를 요청해 임시 파일(§5.3)로 풀고, 그 경로로 `ShellExecuteEx`한다. 별도
  스레드에서 하므로 탐색기 창은 멈추지 않는다.
- 0.4초를 넘기면 셸의 진행 창(`IProgressDialog`)을 띄운다. 진행은 쓰는 중인 바이트, 그 전에는
  아카이브의 압축 해제 진행(`Progress`)이다. 취소하면 아무것도 열지 않고, 쓰던 파일은 서버의
  `Cancel`이 다음 조각에서 멈추고 지운다. 시작하지 않은 파일은 쓰지 않는다. 이미 시작한 leaf의 평가는
  끝까지 가고 캐시에 남는다.
- **옆의 파일과 함께 열기**: 같은 폴더의 파일(하위 폴더는 아니다)과, 파일 이름을 딴 폴더
  (첫 `.` 앞까지의 이름 뒤에 글자나 숫자가 아닌 것: `page.html`의 `page_files`, `page.files`) 아래의
  파일을 같이 푼다. `.cue`와 `.bin`, 영상과 자막, 저장한 웹 페이지와 그 이미지를 위한 것이다.
  합이 256 MiB를 넘으면 먼저 묻는다. 처음에는 폴더 전체를 하위 폴더까지 풀었는데, 루트의 파일에
  쓰면 아카이브 전체가 풀려서 이렇게 좁혔다.
- 실패하면 그 메시지를 상자로 보여 준다.

### 6.4 파일 대화상자

- 신형 `IFileDialog`는 우리 폴더를 탐색할 수 있다. 셸 항목을 받은 프로그램은
  `IShellItem::BindToHandler(BHID_Stream)`으로 읽는다(`BindToObject(IID_IStream)`).
  `FOS_FORCEFILESYSTEM`인 프로그램은 고를 수 없다.

### 6.5 아카이브가 바뀌었을 때

- DLL의 MTA 스레드가 2초마다 쓰고 있는 아카이브 파일의 크기와 시각을 본다. 바뀌었으면 그 아카이브를
  놓고(다음 요청에 새로 연다), 그 아카이브의 모든 폴더 보기에 `SHCNE_UPDATEDIR`을 보낸다. 폴더를
  다시 열거할 때(F5)도 같은 검사를 한다.
- `ReadDirectoryChangesW`로 감시하지는 않는다. 2초의 polling이 단순하고 충분했다.

### 6.6 썸네일과 미리 보기

- 파일의 연결(`IQueryAssociations`)은 확장자, `SystemFileAssociations`의 확장자와 perceived type
  (`image`, `text`: 많은 형식의 handler가 여기 있다), `*`로 만든다. 미리 보기 창은 이것으로 handler를
  찾고 파일은 `BHID_Stream`으로 읽는다.
- 썸네일은 형식의 thumbnail provider(`ShellEx\{e357fccd-...}`)에 파일의 바이트를 메모리 스트림으로
  준다(64 MiB까지). 다른 아파트의 서버 스트림을 주면 provider가 자기 스레드에서 읽다가 호출자를
  기다리게 된다.
- 탐색기는 항목의 그림을 image factory(`IShellItemImageFactory`, 썸네일 캐시)로 가져오는데, 이것은
  폴더에 `IThumbnailProvider`가 아니라 `IExtractImage`를 크기마다 요청한다. 그래서 둘 다 준다
  (`IExtractImage2`: 크기는 `GetLocation`에서, 그림은 `Extract`에서 provider로, 날짜는 항목의
  시각). 처음에 `IThumbnailProvider`만 줬을 때는 보기 크기에 따라 나오기도 하고 안 나오기도 했다.

### 6.7 속성

- 항목에 `SFGAO_HASPROPSHEET`를 주고, 메뉴의 속성(`DFM_CMD_PROPERTIES`, 32비트로 온다:
  `0xfffffffb`)과 Alt+Enter에 우리 시트를 띄운다. 고른 항목마다 한 페이지이고, 열에 값이 있는 것을
  한 줄씩, 위치, formula와 명령어 수까지 보여 준다. 별도 스레드, 별도 창이다.

### 6.8 판(journal의 지점)

판을 하위 폴더로 보이면 아카이브 안의 진짜 디렉터리와 헷갈린다. 그래서 목록에는 넣지 않고, 들어가는
길과 들어간 뒤의 모습을 나눴다.

- 들어간 뒤: 아카이브 파일의 PIDL 뒤에 지점 항목(§3.2)이 붙은 곳이다. 그 아래는 그 지점의 아카이브를
  읽기 전용으로 보여 준다. 열기, 복사, 썸네일, 속성이 그대로 된다. 주소 표시줄에는
  `x.aqr › 라벨 (시각)`으로 보인다.
- 들어가는 길
  1. 보기의 배경 메뉴 "버전 ▸"(`src/versions.rs`, `CreateViewObject(IContextMenu)`): "최신"과
     지점들(최근 것부터). 지금 보는 것에 체크가 붙는다. 지점이 없는 아카이브에는 메뉴를 넣지 않는다.
     고르면 같은 창에서 간다(사이트의 `SID_STopLevelBrowser`, 없으면 창의 `CWM_GETISHELLBROWSER`,
     그것도 없으면 새 창).
  2. 아카이브 파일 속성의 "버전" 탭(`src/history.rs`, property sheet handler): 지점들(최근 것부터).
     탭이 보일 때 읽는다(암호화된 아카이브가 속성을 열 때마다 암호를 묻지 않게). "열기"(더블클릭)는
     그 지점을 새 창으로 연다. "풀기…"는 `aqr-win extract-version ARCHIVE #N`, "이 버전에 이름
     붙이기…"는 `aqr-win name-version ARCHIVE`이고, 이름을 붙이고 나면 목록을 다시 읽는다.
  3. 탐색기 도구 모음(`IExplorerCommandProvider`, 예정. Windows 10과 11에서 어디에 보이는지 먼저
     확인한다).

## 7. 암호와 손상

- **암호화된 아카이브**
  - 서버(aqr-win)가 `passphrase.rs`의 대화상자로 묻는다. 부모는 DLL이 넘긴 셸의 창이고, DLL은
    `CoAllowSetForegroundWindow`로 서버 창이 앞으로 올 수 있게 한다. 틀리면 다시 묻고, 취소하면 그
    폴더는 조용히 실패한다.
  - 아카이브 객체가 암호를 갖고 있다가 `aqr --serve`가 다시 뜨면 그것으로 다시 연다. 키는 handle을
    닫을 때까지 `aqr --serve`의 메모리에 있다.
- **손상된 아카이브**
  - 메타데이터를 읽지 못하면 열거가 실패한다. DLL이 `aqr`의 메시지를 상자로 보여 주고(같은 것은
    10초에 한 번), 셸에는 `ERROR_CANCELLED`를 돌려 탐색기가 오류를 또 보이지 않게 한다.
  - salvage(`open_salvaging`)로 읽은 부분만 보일지는 §10에 남긴다.

## 8. 성능 목표와 측정

아직 재지 않았다. 목표는 이렇다.

- 열거: 1만 항목 디렉터리의 첫 표시가 0.5초 이하.
- 끌어 내보내기: 처리량이 `aqr -d`로 같은 파일을 푸는 것의 90% 이상. 넘지 못하면 표준 출력 대신
  공유 메모리(파일 매핑)로 데이터를 넘긴다.
- 메모리: 서버의 상주량을 `--stats`처럼 기록한다. 보기가 다 닫히고 1분 뒤에는 메타데이터만 남아야
  한다.
- 10gb 같은 큰 아카이브의 첫 열기 시간, 메타데이터 블록을 읽는 시간. "데이터", "비율" 열을 처음 켤
  때 블록을 다 읽는 시간.

## 9. 단계

1. ~~`aqr --serve`~~: 프로토콜, 열린 아카이브, leaf 캐시, spill, 암호화, 진행, `-t -v`의 값.
2. ~~aqr-win --serve~~: COM 클래스, 수명, `IStream`, 재연결, `Materialize`, 암호 대화상자, 다른
   빌드로의 교체, 종료 이벤트.
3. ~~aqr-shell.dll 열어 보기~~: 등록, PIDL, `IShellFolder2`, DefView, 열, 아이콘.
4. ~~끌어 내보내기, 복사~~: 셸의 data object, 다른 프로그램용 descriptor와 contents.
5. ~~열기~~: 임시 파일, 진행 창, 취소, 옆의 파일과 함께, 컨텍스트 메뉴.
6. ~~암호화된 아카이브, 손상의 표시, 바뀐 아카이브의 감시~~.
7. ~~썸네일과 미리 보기, 속성 시트~~.
8. ~~설치 프로그램과 릴리스~~(§12).
9. 그 뒤:
   - 판(journal의 지점, §6.8): 배경 메뉴와 속성 탭(완료), 도구 모음
   - 다른 형식(aqr-unpack이 읽는 것)과 넣기(`IDropTarget` → `aqr -a`)
   - 손상된 아카이브의 salvage
   - ProjFS로 "폴더로 마운트"
   - Windows 11의 새 컨텍스트 메뉴(aqr-win README의 Status)
   - ARM64(EC)
   - 측정(§8)

## 10. 열린 질문

- ~~더블클릭의 기본~~: DLL이 있으면 열어 보기, 없으면 지금처럼 "여기에 풀기"로 정했다(§3.1).
- ~~서버 하나 대 아카이브마다~~: 서버 하나가 모든 아카이브를 맡는다. 단순하고 캐시 상한을 나눌 필요가
  없다. 대신 한 아카이브의 크래시가 다른 것에 번진다(DLL은 다시 연결한다). `progress`가 요청별로
  나뉘지 않는 것도 이 때문이다.
- mmap과 `ReadFile`: 열린 아카이브를 잠그지 않으려면 `ReadFile`이 낫다. 그러면 `Archive`가 `&[u8]`
  전체를 받는 지금의 API를 어떻게 할지 정해야 한다.
- 손상된 아카이브에서 salvage한 부분을 보일지.
- Windows 7의 `ITransferSource`: 구현하면 Windows 7에서도 셸의 data object를 쓸 수 있다. 지금은
  descriptor로 충분하다.
- `IDataObjectAsyncCapability`: 셸의 data object에 맡겼다. 큰 leaf를 평가하는 동안 다른 프로그램으로
  끄는 쪽에서 UI가 멈추는지는 확인하지 않았다.

## 11. 테스트

- `crates/aqr/tests/serve.rs`: 프로토콜. 목록이 `aqr -t --json`과 같은지, `read`한 바이트가 원본과
  같은지, spill, 동시 요청, handle, 암호화, `verbose`. 플랫폼과 무관하다.
- `crates/aqr-win/tests/serve.rs`: COM 서버를 다른 프로세스에서 부른다. 테스트가 서버를 직접 띄우고
  자기 CLSID로 등록하게 하므로(실행 중인 서버의 등록만으로 활성화된다) 레지스트리를 건드리지 않는다.
  `aqr.exe`가 옆에 빌드되어 있어야 해서 ignored다. 다른 빌드로의 교체(`superseded`)와 종료
  이벤트(`quit`)는 ignored가 아니다.
- `crates/aqr-win-shell/tests/shell.rs`: 같은 프로세스에서 셸을 거친다. 테스트용 클래스와 `.aqrtest`
  형식을 HKCU에 잠시 등록하고, 서버는 이번 실행만의 CLSID로 띄운다(`AQR_SHELL_TEST_SERVER`). 파싱,
  열거, 스트림, 열, 복사 엔진의 복사, 클립보드, 메뉴, 옆의 파일과 함께 열기, 암호화, 바뀐 아카이브,
  썸네일(image factory로, 여러 크기), 속성. 실패해도 서버가 남지 않게 죽인다. ignored다.
- 테스트 클래스(`TEST_FOLDER`)에서는 DLL이 상자를 띄우지 않고 셸에 변경을 알리지 않는다
  (`TESTING`). 알리면 사용자의 탐색기가 테스트의 DLL을 로드한다. 대신 `AQR_SHELL_LOG`에 적는다.
  `AQR_SERVE_TEST_PASSPHRASE`, `AQR_SHELL_TEST_OPENED`, `AQR_SERVE_IDLE`, `AQR_SERVE_QUIT_EVENT`는
  대화상자, 실행, 시간, 종료 이벤트를 테스트가 대신한다.
- 손으로만 확인한 것: 진행 창, 256 MiB 확인, 실제 `ShellExecuteEx`, 속성 시트의 창 자체.
- Windows 7: 위의 테스트를 win7 target으로 빌드해 VM(Server 2008 R2, NT 6.1)에서 돌린다. 셸
  테스트는 대화형 세션이 필요해서 예약 작업(`schtasks /it`)으로 띄운다. 붙여 넣기와 끌기는 폴더의
  drop target에 놓아서 시험하고(두 Windows에서), 복사 엔진에 직접 맡기는 것은 Windows 8 이후에서만
  시험한다.

## 12. 설치와 제거

- 설치 프로그램(`crates/aqr-win-install`)이 `aqr.exe`, `aqr-win.exe`, `aqr-shell.dll`을 함께 싣는다.
  릴리스는 `cargo +NIGHTLY win7 -p aqr -p aqr-win -p aqr-win-shell`로 만든다.
- 업데이트: 설치 프로그램은 먼저 종료 이벤트로 서버들을 끝낸다. 그러지 않으면 실행 중인 서버의
  파일이 `.old`로 비켜지고, 그 서버는 등록된 경로가 자기 것과 같아서 교체된 줄 모르고 옛 코드로 계속
  응답한다(VM에서 겪었다). 탐색기가 로드한 이전 DLL은 지울 수 없지만 이름은 바꿀 수 있으므로 `.old`로
  비켜 놓고 새 것을 쓴다. 탐색기는 다시 시작할 때까지 이전 것을 쓰므로(DLL은 pin되어 있다) 설치
  프로그램이 그렇게 알려 준다.
- 제거(`aqr-win --uninstall`): 먼저 종료 이벤트로 서버들을 끝낸다(`aqr --serve`의 aqr.exe가 실행
  중이면 지울 수 없다). 탐색기가 잡고 있는 DLL은 임시 디렉터리로 옮기고, 다음 로그인 때 지우도록
  `RunOnce`에 적는다(값의 이름은 옮긴 파일의 것: 제거가 거듭돼도 앞의 것을 덮지 않는다). 그 뒤로는
  aqr-win.exe를 `cmd.exe`가 지우는 이전 방식이다.
- Windows 7 VM에서 설치, 설치된 것으로 열어 보기와 복사, DLL을 잡은 채 업데이트, 제거, 다음
  로그인의 삭제까지 확인했다.

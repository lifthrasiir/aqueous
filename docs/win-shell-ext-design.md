# 탐색기 안에서 아카이브 열어 보기 (Windows shell namespace extension)

`.aqr` 파일(과 aqr-unpack이 읽는 다른 형식)을 탐색기 안에서 폴더처럼 열고, 끌어 내보내고, 더블클릭으로
여는 기능이다. Windows의 ZIP 폴더(`zipfldr.dll`)와 같은 자리다. 구현은 끝났고(열어 보기, 복사, 열기,
암호, 썸네일, 속성, 판, 넣기, 다른 형식, 설치), 이 문서는 지금 코드를 바꿀 때 필요한 결정과 이유, 남은
일만 적는다. 끝낸 일의 경과는 git 이력에 있다. 구현은 세 crate에 나뉜다: `crates/aqr/src/serve.rs`
(`aqr --serve`, 프로토콜은 그 모듈 문서), `crates/aqr-win/src/serve.rs`(`aqr-win --serve`),
`crates/aqr-win-shell`(`aqr-shell.dll`, README와 각 파일의 모듈 문서).

## 1. 방식의 선택

| 방식 | 판단 |
|---|---|
| VirtDisk | ISO, VHD, VHDX만 받는다. 내용을 콜백으로 공급할 수 없다 |
| ProjFS | 선택 기능이라 켜야 하고, 읽은 파일은 디스크에 hydrate된다 |
| Cloud Files (cfapi) | sync root 등록과 정리가 무겁고, 동기화 상태 아이콘이 뜬다 |
| WinFsp, Dokany | 드라이버 설치가 필요하고, 라이선스가 퍼블릭 도메인 배포와 맞지 않는다(설치된 WinFsp에 마운트하는 것은 별도로 배포하는 `crates/aqr-mount-win`) |
| 별도 창 (7-Zip, WinRAR) | 탐색기와 똑같이 만들 수 없다 |
| **Shell namespace extension** | **채택** |

DefView(`SHCreateShellFolderView`)를 그대로 쓰므로 보기 모드, 정렬, 열, 검색, 주소 표시줄이 탐색기와
같다. 한계는 파일 시스템이 아니라는 점이다. 경로로 파일을 여는 프로그램에는 임시 파일로 풀어 준다(§6.3).
ProjFS 투영은 나중에 "폴더로 마운트" 동사로 덧붙일 수 있고, 그 경우에도 §4의 서버와 캐시는 그대로 쓴다.

## 2. 구성

```text
Explorer 또는 파일 대화상자를 연 아무 프로세스
  └ aqr-shell.dll (in-proc, 얇게)       IShellFolder2, PIDL, IDataObject, 메뉴, 열, 썸네일
        │ COM (CLSCTX_LOCAL_SERVER, IDispatch)
  aqr-win --serve (COM local server)   활성화, 수명, 암호 대화상자, IStream, Materialize
        │ 표준 입출력 (aqr --serve 프로토콜)
  aqr --serve                          아카이브 열기, 디코딩, 캐시
```

- **aqr-shell.dll**은 공용 파일 대화상자를 여는 모든 프로세스에 로드되므로 디코더, Wasm 엔진, 큰 캐시를
  넣지 않는다. 남의 프로세스가 같이 죽지 않게 모든 COM 진입점에서 panic을 `catch_unwind`로 잡아 실패로
  바꾼다. cargo의 결과는 `aqr_shell.dll`이고 배포 이름은 `aqr-shell.dll`이다(복사할 때 바꾼다).
- **aqr-win --serve**는 `LocalServer32`이고 GUI 서브시스템이라 콘솔 창이 뜨지 않는다. 디코더는 링크하지
  않고 `aqr --serve` 자식 하나(`CREATE_NO_WINDOW`)에 맡긴다. 모든 아카이브가 그 하나를 같이 쓴다(§10).
- **aqr --serve**는 디코딩과 캐시를 맡는다(코덱, 네이티브 모듈, 샌드박스가 이미 있다). 표준 입출력
  프로토콜이라 COM 없이 테스트하고(`crates/aqr/tests/serve.rs`) macOS File Provider에서도 다시 쓸 수 있다.
  프론트엔드 쪽 구현은 `aqr_ipc::serve`다.

aqr.exe를 직접 COM 서버로 두지 않는 이유: 콘솔 서브시스템이라 COM이 띄우면 콘솔 창이 생기고, COM 코드가
CLI에 섞인다. 데이터가 한 단계 더 복사되는 비용은 디코딩에 비하면 작다. 문제가 되면 §8의 공유 메모리로
줄인다.

## 3. 셸 쪽 (aqr-shell.dll)

### 3.1 등록

HKCU에만 등록한다(관리자 권한 없음). `aqr-win --register`(설치 프로그램은 `--install`)가 `aqr-shell.dll`이
aqr-win.exe 옆에 있을 때만 등록하고 없으면 지운다. 목록은 `crates/aqr-win/src`의 등록 코드에 있고, 결정만
적는다.

- 폴더 클래스에는 `Implemented Categories\{00021490-...}`(CATID_BrowsableShellExt)가 필요하다. 없으면 셸이
  파일을 junction으로 받지 않는다(ZIP 폴더도 둔다). `Attributes`는 `SFGAO_FOLDER | SFGAO_STREAM |
  SFGAO_DROPTARGET`이다.
- **더블클릭은 열어 보기다.** ProgID의 기본 동사 `open`에 `DelegateExecute = {11dbb47c-...}`
  (CLSID_ExecuteFolder)를 둔다. "여기에 풀기"와 "검사"는 동사로 남는다. 그러면 "연결 프로그램"에 셸 자신
  (ExplorerFrame)의 이름이 보이므로 ProgID에 `Application\ApplicationName = Aqueous`(와 `ApplicationIcon`)를
  둔다. DLL이 없으면 "여기에 풀기"가 기본이다.
- 다른 형식(§6.10)의 ProgID(`Aqueous.zip`, ...)에도 같은 `CLSID`와 `open`을 둔다. 확장자의 기본 ProgID는
  다른 프로그램이 없을 때만 우리 것이므로, `.zip`은 사용자가 고르기 전까지 Windows의 ZIP 폴더다.
- 속성의 "버전" 탭(§6.8)과 넣기의 drop handler(§6.9)는 각자 클래스를 둔다.

### 3.2 PIDL

아카이브 안 항목 하나가 `SHITEMID` 하나다(`src/pidl.rs`; 바깥 부분은 아카이브 파일까지의 파일 시스템
PIDL). PIDL은 즐겨찾기, 최근 항목, 점프 목록에 오래 남으므로 `magic 'AQRI'`와 버전(1)을 붙여 형식을
고정했다. 필드: kind(dir, file, link, other, journal 지점), size, mtime, Windows 특성, 이름.

- 항목은 이름으로만 식별한다. content index는 넣지 않는다(`-a`로 index가 바뀌므로). 크기와 시간은 열을
  빨리 채우는 사본이다.
- 이름은 `aqr -d`가 쓰는 이름 그대로다(`crates/aqr/src/names.rs`, 서버가 준다). 내보낸 이름과 풀어 쓴
  이름이 같다.
- journal 지점(판, kind 4)은 아카이브 파일 바로 다음에만 오고, 그 뒤 항목은 그 지점 아카이브의 것이다.
  size는 지점의 오프셋(서버의 `metadata`)이다. 목록에는 나오지 않고 메뉴로만 간다(§6.8). 파싱 이름은
  `x.aqr\@1234\a.txt`이고, 루트에 같은 이름의 항목이 있으면 항목이 먼저다.

### 3.3 인터페이스

`IShellFolder2`/`IPersistFolder3`(열거, `BindToObject`, `CompareIDs`, 열), `CreateViewObject` →
`SHCreateShellFolderView`, 열(`src/columns.rs`: `aqr -t -v`의 모든 것, 기본은 이름, 수정한 날짜, 유형, 크기.
시스템 속성이 있는 열은 그 키를 써서 셸이 이름 붙이고, 나머지는 우리 FMTID로 aqr-l10n이 붙인다),
`IContextMenu`(`src/menu.rs`: `SHCreateDefaultContextMenu`에 콜백을 더한다), `IDataObject`(§6.2),
`IQueryAssociations`/`IThumbnailProvider`/`IExtractImage2`(§6.6), 속성 시트(§6.7). 아카이브 자체의
`IDropTarget`은 §6.9.

- 셸 쪽은 `windows` crate의 `#[implement]`, aqr-win 쪽은 객체가 셋뿐이라 `windows-sys`로 vtable을 손으로
  짰다.
- x86-64만이다(ARM64 탐색기는 x64 DLL을 로드하지 못한다).
- 릴리스는 win7 target이다. 그런데 `windows` crate가 Windows 8 이후 DLL을 링크하므로(windows-core의
  `combase.dll`: `CoTaskMemAlloc`, `CoTaskMemFree`, `CoCreateFreeThreadedMarshaler`; windows-result의
  `api-ms-win-core-winrt-error-l1-1-0.dll`: `RoOriginateErrorW`) 둘을 delay-load하고(`build.rs`), hook
  (`src/delay.rs`)이 `combase.dll`이 없으면 `ole32.dll`을, `RoOriginateErrorW`가 없으면 빈 함수를 준다.

### 3.4 스레드와 아파트

- 탐색기는 창마다 STA 스레드를 두고 DefView는 열거와 썸네일을 백그라운드 스레드에서도 부른다.
- **서버 객체는 프로세스의 MTA가 가진다**(`src/client.rs`). Global Interface Table에 넣었더니 쿠키가 등록한
  아파트(처음 연 창의 스레드)에 속해서, 그 창을 닫으면 항목이 사라지고 다른 곳에서 같은 아카이브를 쓸
  때 `E_INVALIDARG`가 났다. 그래서 DLL의 스레드 하나가 MTA를 프로세스 수명 동안 붙들고(DLL은 pin한다)
  서버 호출은 언제나 MTA 스레드에서 한다. 호출자 아파트로 넘길 스트림만 GIT로 그 자리에서 건넨다.
- 잠금을 잡은 채 COM 호출을 하지 않는다(STA의 바깥 호출은 메시지를 돌려 재진입한다).
- 서버가 없어지면(`RPC_E_DISCONNECTED`, 이전 빌드의 `DISP_E_UNKNOWNNAME`) 가진 것을 버리고 한 번 다시
  시도한다.

## 4. 서버

### 4.1 aqr-win --serve

- `REGCLS_MULTIPLEUSE`. **타입 라이브러리 없는 `IDispatch`**라서 시스템 프록시가 마샬링하고 proxy/stub DLL이
  필요 없다. 메서드(문자열 인자, JSON 결과): 클래스 `Open(path[, hwnd[, at]])`(`at`: journal 지점의
  오프셋), 아카이브 `Info`, `List`, `Stat`, `OpenStream` → `IStream`, `Materialize`, `Progress`, `Cancel`,
  `Journal`.
- 아카이브 객체가 놓이면 handle은 1분 뒤에 닫는다(탐색기가 폴더를 옮길 때마다 객체를 만들었다 놓기
  때문). 객체가 없이 10분이 지나면 클래스를 revoke하고 끝난다. 수치는 정해 둔 것이니 재 보고 고친다.
- 2초마다 `LocalServer32`가 자기 실행 파일을 가리키는지 본다. 다른 빌드로 등록됐거나 지워졌으면 revoke하고
  객체가 없어지는 즉시 끝난다(이전 빌드의 서버가 새 DLL을 받는 일을 막는다). 세션 이벤트
  `Local\Aqueous.Serve.Quit`이 켜져도 곧바로 끝난다(§12).
- 서버가 죽으면 DLL이 다시 만든다. 열려 있던 `IStream`은 실패하고 복사 대화상자가 오류를 보인다.
- 확인하지 않은 것: RDP 세션 여럿이나 사용자 전환에서 따로 뜨는지, 관리자 권한(high IL) 프로세스의 파일
  대화상자에서 부를 때 별도 인스턴스가 뜨는지.

### 4.2 aqr --serve (프로토콜)

한 줄에 JSON 하나(`--ipc`처럼), 모든 요청에 `id`, 응답 순서는 바뀔 수 있다. 요청: `hello`, `open`,
`list`, `stat`, `read`, `progress`, `journal`, `trim`, `close`. 전체는 `crates/aqr/src/serve.rs`의 모듈
문서다. 결정만 적는다.

- `open`은 같은 파일(경로와 identity)이면 같은 handle이다. 암호는 `passphrase`로 받고, 없으면 `encrypted`,
  틀리면 `wrong` 오류다.
- `list`/`stat`의 `verbose` 값(`data`, `overhead`, `ratio`, ...)은 블록을 다 읽어야 해서 처음 한 번 만들어
  둔다. 디렉터리는 4096개씩 나눠 보낸다.
- `read`는 한 번에 1 MiB까지다(큰 읽기 하나가 다른 응답을 오래 막지 않게). `progress`는 요청별로 나뉘지
  않는다(§10).
- `open`의 `at`은 journal 지점의 `metadata`이고, 그 지점의 아카이브(그 뒤의 지점을 모른다)를 따로 연다.

## 5. 캐시

- **열린 아카이브**: 키는 파일 정체(볼륨 일련번호 + 파일 ID), 크기, 수정 시각. 가진 것은 mmap과 그 위의
  `Archive`(`open_indexed`: 메타데이터 블록만), 암호화된 것은 복호화 중인 payload다. `Archive`가 빌리므로
  빌린 것들을 leak해 두고 필드 순서대로 놓는다(`Leaked`). 열린 동안 mmap 때문에 파일의 잘라내기와
  덮어쓰기가 실패한다. 보기가 다 닫히고 1분 뒤 놓는다. 그 전에 `-a`가 쓰지 못하면 그 오류가 그대로
  보인다(`ReadFile`로 바꿀지는 §10).
- **내용**: 단위는 leaf다. 원하는 content가 든 leaf만 평가하고(`each_leaf_only`) 그 출력을 모두 둔다
  (탐색기는 같은 폴더의 파일을 이어 읽는다). LRU는 바이트 상한(기본 물리 메모리의 1/8, 1 GiB 이하,
  `-Z serve:cache`), 키는 (아카이브, leaf)이고 같은 leaf의 동시 요청은 한 번만 평가한다. 상한보다 큰
  출력은 임시 파일로 spill한다. 다만 평가하는 동안 leaf 전체가 메모리에 한 번 올라온다. 근본 해결은
  aqr-archive에 트리 아카이브 content를 흘려 쓰는 경로를 두는 것이다(`write_file`의 `eval::run`식. 단일
  파일 아카이브는 이미 흘려 쓴다).
- **leaf 하나의 비용**: 작은 파일 하나에 큰 solid leaf 전체를 풀어야 할 수 있다. `docs/crates.md`의 남은 과제
  "top-level 엔트리 의존 closure의 바이트 상한"이 여기서 체감 지연으로 드러난다.
- **임시 파일**: `Materialize`(§6.3)는 `%LOCALAPPDATA%\Aqueous\Open\<경로와 identity의 해시>\<아카이브 안
  경로>`에 `.part`로 쓰고 이름을 바꾼다. 수정 시각은 항목의 것이고 읽기 전용 특성을 붙인다(임시 사본을
  고쳐 저장했다가 잃는 일을 막는다). 같은 크기의 파일이 있으면 다시 쓰지 않는다. 서버가 끝날 때 지우고
  (열려 있는 것은 남는다) 시작할 때 하루 지난 것을 지운다.

## 6. 동작

열거는 `list`를 보내 받은 항목을 이름별로 두고, 열 값은 PIDL이나 둔 항목에서 채워 칸마다 서버를 부르지
않는다. 데이터, 비율 같은 열이 처음 필요하면 그 폴더를 `verbose`로 한 번 더 받는다.

### 6.2 끌어 내보내기, 복사

- 셸의 data object(`SHCreateDataObject`, ID list)를 준다. Windows 8 이후 복사 엔진은 폴더와
  스트림(`BHID_Stream`)으로 읽으므로 붙여 넣기와 탐색기 폴더로의 끌기는 이것만으로 되고, 진행 창도 복사
  엔진의 것이다.
- **Windows 7**의 복사 엔진과 폴더 drop target은 ID list 항목을 원본 폴더가 `ITransferSource`를 줄 때만
  받는다. 그래서 7에서는 안쪽 객체(descriptor와 contents)만 준다. drop target이 Outlook의 첨부처럼 받아
  스스로 복사한다(VM에서 확인).
- 셸이 아닌 프로그램(브라우저 업로드, 메일 첨부)에는 안쪽 객체가 `CFSTR_FILEDESCRIPTORW`(고른 것과 그 아래
  모든 항목)와 `CFSTR_FILECONTENTS`(index마다 서버의 `IStream` 프록시, `Read` 하나가 RPC 왕복 하나, 1 MiB씩)
  를 준다. `CF_HDROP`(실제 경로)은 주지 않는다.

### 6.3 열기

- `Materialize`로 임시 파일(§5)에 풀고 그 경로로 `ShellExecuteEx`한다(별도 스레드). 0.4초를 넘기면
  `IProgressDialog`를 띄운다(쓰는 중인 바이트, 그 전에는 `Progress`). 취소하면 아무것도 열지 않고, 쓰던 파일은
  `Cancel`이 다음 조각에서 멈추고 지운다. 이미 시작한 leaf의 평가는 끝까지 가서 캐시에 남는다. 실패하면
  메시지를 상자로 보인다.
- **옆의 파일과 함께 열기**: 같은 폴더의 파일(하위 폴더 제외)과 파일 이름을 딴 폴더(첫 `.` 앞의 이름 뒤에
  글자나 숫자가 아닌 것: `page.html`의 `page_files`) 아래의 파일을 같이 푼다(`.cue`/`.bin`, 영상과 자막, 저장한
  웹 페이지). 합이 256 MiB를 넘으면 먼저 묻는다. 처음에는 하위 폴더까지 전부 풀었는데 루트의 파일에서
  아카이브 전체가 풀려 좁혔다.
- 신형 `IFileDialog`는 우리 폴더를 탐색하고 `BindToHandler(BHID_Stream)`으로 읽는다.
  `FOS_FORCEFILESYSTEM`인 프로그램은 고를 수 없다.

### 6.5 아카이브가 바뀌었을 때

DLL의 MTA 스레드가 2초마다 쓰고 있는 아카이브 파일의 크기와 시각을 보고, 바뀌었으면 놓고(다음 요청에 새로
연다) 그 아카이브의 모든 폴더 보기에 `SHCNE_UPDATEDIR`을 보낸다. 다시 열거할 때(F5)도 같은 검사를 한다.
`ReadDirectoryChangesW`보다 2초 polling이 단순하고 충분했다.

### 6.6 썸네일과 미리 보기

- 파일 연결(`IQueryAssociations`)은 확장자, `SystemFileAssociations`의 확장자와 perceived type, `*`로
  만든다. 미리 보기 창은 이것으로 handler를 찾고 `BHID_Stream`으로 읽는다.
- 썸네일은 형식의 thumbnail provider에 파일 바이트를 메모리 스트림으로 준다(64 MiB까지; 다른 아파트의
  서버 스트림을 주면 provider가 자기 스레드에서 읽다가 호출자를 기다린다).
- image factory(`IShellItemImageFactory`)는 폴더에 `IThumbnailProvider`가 아니라 `IExtractImage`를 크기마다
  요청한다. 그래서 둘 다 준다(`IExtractImage2`). `IThumbnailProvider`만 줬을 때는 보기 크기에 따라 나오기도
  하고 안 나오기도 했다.

### 6.7 속성

`SFGAO_HASPROPSHEET`를 주고, 메뉴의 속성(`DFM_CMD_PROPERTIES`: 32비트로 온다, `0xfffffffb`)과 Alt+Enter에
우리 시트(`src/props.rs`: 고른 항목마다 한 페이지)를 별도 스레드, 별도 창으로 띄운다.

### 6.8 판(journal의 지점)

판을 하위 폴더로 보이면 아카이브 안의 진짜 디렉터리와 헷갈리므로 목록에는 넣지 않고, 들어가는 길과 들어간
뒤의 모습을 나눴다. 들어간 뒤는 PIDL에 지점 항목(§3.2)이 붙은 곳이고, 그 아래는 그 지점의 아카이브를 읽기
전용으로 보인다(주소 표시줄 `x.aqr › 라벨 (시각)`). 들어가는 길:

1. 보기 배경 메뉴 "버전 ▸"(`src/versions.rs`): "최신"과 지점들. 고르면 같은 창에서 간다. 지점이 없으면
   메뉴를 넣지 않는다.
2. 아카이브 속성의 "버전" 탭(`src/history.rs`): 열기(새 창), 풀기…(`aqr-win extract-version`), 이름
   붙이기…(`name-version`), 이후 버전 삭제…(`rollback-version`: 제자리에서 자르거나 `aqr --rollback`, 그
   지점을 옆에 새 아카이브로 저장하거나 `aqr --convert --keep-until`). 탭이 보일 때 읽는다(암호화된
   아카이브가 속성마다 암호를 묻지 않게). 삭제는 되돌릴 수 없어 속성 안에만 둔다.
3. 탐색기 도구 모음의 "버전" 명령(`src/commands.rs`, `IExplorerCommandProvider`). Windows 7의 명령 모음에는
   나온다. Windows 10 리본과 11의 새 명령 모음에 나오는지는 확인하지 않았다.

### 6.9 넣기(끌어 놓기, 붙여넣기)

- 받는 곳: 보기(`CreateViewObject(IDropTarget)`), 보기 안의 폴더(`GetUIObjectOf`), 일반 폴더의 아카이브
  파일(ProgID의 `shellex\DropHandler`, `IPersistFile`). 붙여넣기는 보기 자신의 "붙여넣기"다. DefView가 받는
  폴더(`SFGAO_DROPTARGET`)의 배경 메뉴에 넣으므로 우리 메뉴에도 넣었더니 두 개가 되어, 이름으로 부르는
  동사 `paste`만 남겼다(`src/versions.rs`).
- 파일 시스템의 파일(`CF_HDROP`)만 받는다(다른 아카이브나 메일의 가상 파일은 받지 않는다). 언제나 복사이고
  아카이브 자신이나 그것을 담은 폴더는 뺀다. 판의 보기는 받지 않는다. 암호화된 아카이브는 `aqr`가 거절하고
  aqr-win이 그 오류를 보인다(nonce 재사용, `docs/append-design.md`).
- `aqr-win add ARCHIVE DIR PATH...`가 `aqr --ipc -a -o ARCHIVE PATH --rename DIR/NAME ...`을 진행 창과 함께
  돌린다(`aqr_ipc::command_add`; `--rename`을 늘 붙이므로 폴더를 놓으면 그 내용이 아니라 폴더가 들어간다).
  같은 이름이 있으면 먼저 묻는다(`-a`는 바꿔 넣고 예전 것은 앞의 segment에 남아 판에서 보인다). 끝나면 §6.5의
  감시가 새로 고친다.

### 6.10 다른 형식

aqr-unpack이 읽는 것(ZIP, 7z, RAR, ALZ, EGG, tar와 압축기, 단일 파일 스트림)을 같은 폴더 클래스가 보인다.
서버가 이름으로 형식을 알아보고 aqr-unpack으로 연다. ZIP, 7z, RAR, ALZ, EGG는 목록만 읽고(`Opened::sizes`),
읽기는 파일 하나만 푼다(`Opened::unpack_selected`). tar는 열 때 한 번 다 읽고, 처음 읽을 때 전부 풀어 하나의
leaf로 둔다. 단일 파일은 압축기 확장자를 뗀 이름의 아카이브로 보인다. 암호(ZIP, 7z)는 `.aqr`처럼 서버의
대화상자가 묻는다. 열은 `open`의 답의 `fields`로 형식이 가진 것만 보인다(이름, 형식, 크기는 항상. 파일을
따로 압축하는 형식은 데이터, 비율도. 오버헤드, 세그먼트, 주석, macOS 플래그는 `.aqr`만). 넣기, 판, 버전 탭은
없다(`client::appendable`: `fields`가 없는 아카이브만).

## 7. 암호와 손상

- 암호화된 아카이브: 서버(aqr-win)가 `passphrase.rs`의 대화상자로 묻는다(부모는 DLL이 넘긴 셸 창, DLL이
  `CoAllowSetForegroundWindow`로 앞으로 올린다). 틀리면 다시 묻고, 취소하면 그 폴더는 조용히 실패한다.
  아카이브 객체가 암호를 갖고 있다가 `aqr --serve`가 다시 뜨면 그것으로 다시 연다.
- 손상: 메타데이터를 읽지 못하면 열거가 실패한다. DLL이 `aqr`의 메시지를 상자로 보이고(같은 것은 10초에 한
  번) 셸에는 `ERROR_CANCELLED`를 돌려 탐색기가 오류를 또 보이지 않게 한다. salvage(`open_salvaging`)로 읽은
  부분만 보이는 것은 하지 않았다(§10).

## 8. 성능 목표 (아직 재지 않았다)

- 열거: 1만 항목 디렉터리의 첫 표시 0.5초 이하.
- 끌어 내보내기: `aqr -d`로 같은 파일을 푸는 것의 90% 이상. 못 넘으면 표준 출력 대신 공유 메모리(파일
  매핑)로 넘긴다.
- 메모리: 서버의 상주량을 `--stats`처럼 기록하고, 보기가 다 닫히고 1분 뒤에는 메타데이터만 남아야 한다.
- 10gb 같은 큰 아카이브의 첫 열기와 "데이터", "비율" 열을 처음 켤 때 블록을 다 읽는 시간.

## 9. 남은 일

- 판: 도구 모음 명령이 Windows 10, 11에서 보이는지 확인.
- 다른 형식에 넣기(aqr-pack으로 다시 쓰기). 넣기의 가상 파일(다른 아카이브, 메일 첨부), 지우기(`aqr
  --delete`)와 이름 바꾸기(`--move`).
- 손상된 아카이브의 salvage 보기.
- ProjFS로 "폴더로 마운트"(WinFsp가 설치된 곳에서는 "드라이브로 마운트": `crates/aqr-mount-win`, aqr-win-shell의 `mount`).
- Windows 11의 새 컨텍스트 메뉴(aqr-win README의 Left).
- ARM64(EC).
- 측정(§8).

## 10. 열린 질문과 정한 것

- 서버 하나가 모든 아카이브를 맡는다(단순하고 캐시 상한을 나눌 필요가 없다). 대신 한 아카이브의 크래시가
  다른 것에 번진다(DLL은 다시 연결한다). `progress`가 요청별로 나뉘지 않는 것도 이 때문이다.
- mmap과 `ReadFile`: 열린 아카이브를 잠그지 않으려면 `ReadFile`이 낫다. 그러면 `Archive`가 `&[u8]` 전체를
  받는 지금의 API를 어떻게 할지 정해야 한다.
- 손상된 아카이브에서 salvage한 부분을 보일지.
- Windows 7의 `ITransferSource`: 구현하면 7에서도 셸의 data object와 복사 엔진의 진행 창을 쓸 수 있다.
  지금은 descriptor로 충분하다.
- `IDataObjectAsyncCapability`: 셸의 data object에 맡겼다. 큰 leaf를 평가하는 동안 다른 프로그램으로 끄는
  쪽의 UI가 멈추는지는 확인하지 않았다.

## 11. 테스트

- `crates/aqr/tests/serve.rs`: 프로토콜(플랫폼 무관).
- `crates/aqr-win/tests/serve.rs`: COM 서버를 다른 프로세스에서 부른다. 테스트가 서버를 직접 띄우고 자기
  CLSID로 등록하게 하므로(실행 중인 서버의 등록만으로 활성화된다) 레지스트리를 건드리지 않는다. `aqr.exe`가
  옆에 있어야 해서 ignored이고, `superseded`와 `quit`은 아니다.
- `crates/aqr-win-shell/tests/shell.rs`(ignored): 같은 프로세스에서 셸을 거친다. 테스트용 클래스와 `.aqrtest`
  형식을 HKCU에 잠시 등록하고 서버는 이번 실행만의 CLSID로 띄운다(`AQR_SHELL_TEST_SERVER`). 테스트
  클래스(`TEST_FOLDER`)에서는 DLL이 상자를 띄우지 않고 셸에 변경을 알리지 않는다(`TESTING`: 알리면 사용자의
  탐색기가 테스트 DLL을 로드한다. 대신 `AQR_SHELL_LOG`). `AQR_SERVE_TEST_PASSPHRASE`,
  `AQR_SHELL_TEST_OPENED`, `AQR_SERVE_IDLE`, `AQR_SERVE_QUIT_EVENT`가 대화상자, 실행, 시간, 종료 이벤트를
  대신한다.
- 손으로만 확인한 것: 진행 창, 256 MiB 확인, 실제 `ShellExecuteEx`, 속성 시트의 창.
- Windows 7: win7 target으로 빌드해 VM(Server 2008 R2)에서 돌린다. 셸 테스트는 대화형 세션이 필요해
  `schtasks /it`로 띄운다. 붙여 넣기와 끌기는 폴더의 drop target에 놓아서 시험하고(두 Windows에서), 복사
  엔진에 직접 맡기는 것은 Windows 8 이후에서만 시험한다.

## 12. 설치와 제거

- 설치 프로그램(`crates/aqr-win-install`)이 `aqr.exe`, `aqr-win.exe`, `aqr-shell.dll`을 싣는다(릴리스는 `cargo
  +NIGHTLY win7 -p aqr -p aqr-win -p aqr-win-shell`).
- 업데이트: 먼저 종료 이벤트로 서버들을 끝낸다. 그러지 않으면 실행 중인 서버의 파일이 `.old`로 비켜지고, 그
  서버는 등록된 경로가 자기 것과 같아 교체된 줄 모르고 옛 코드로 계속 응답한다(VM에서 겪었다). 탐색기가
  로드한 DLL은 지울 수 없지만 이름은 바꿀 수 있으므로 `.old`로 비키고 새 것을 쓴다. 탐색기는 다시 시작할
  때까지 이전 것을 쓰므로(DLL은 pin) 설치 프로그램이 알려 준다.
- 제거(`aqr-win --uninstall`): 먼저 종료 이벤트로 서버들을 끝낸다(`aqr --serve`의 aqr.exe가 실행 중이면 지울 수
  없다). 탐색기가 잡은 DLL은 임시 디렉터리로 옮기고 다음 로그인 때 지우도록 `RunOnce`에 적는다(값 이름은
  옮긴 파일의 것: 거듭돼도 앞의 것을 덮지 않는다). aqr-win.exe는 `cmd.exe`가 지운다.
- Windows 7 VM에서 설치, 열어 보기와 복사, DLL을 잡은 채 업데이트, 제거, 다음 로그인의 삭제까지 확인했다.

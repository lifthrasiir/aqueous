# XML의 골격과 값 묶음 (aqr-xml)

레코드로 된 XML(위키·Q&A 사이트의 덤프, 지도의 변경분, 서지)을 골격과 문맥별 값 묶음으로 나누어 묶음마다 그에
맞는 코더가 코딩하게 한다. **[구현]**: 형식은 `codecs/aqr-xml/FORMAT.md`, 프로토타입의 측정과 코퍼스는
`codecs/aqr-xml/README.md`. 이 문서는 정한 것의 이유와 남은 문제다. 끝낸 일의 경과는 git 이력에 있다.

출발점은 cmix-neif-2.0의 enwik9 전처리기(prep9)의 README다(코드는 GPL이라 읽지 않았다). 그 8단계가 페이지마다의
리비전 정보를 본문에서 빼어 파일 앞의 블록 하나에 모으는 것을 enwik9에 매이지 않게 일반화하면 XMill(Liefke,
Suciu 2000)이다: 태그는 골격에, 값은 경로별 컨테이너에.

## 1. 정한 것

### 1.1 모듈은 나누고 합치기만 한다

`xml-recons`는 골격과 묶음에서 XML을 다시 만들 뿐 값을 코딩하지 않는다. 각 묶음은 `Kind::Text`인 내용
스트림이고 aqr-compress가 차례로 다시 압축한다(숫자·시각의 열은 `table`: aqr-xsv, aqr-log; 나머지 텍스트는
aqr-wrt, 묶음 전체에 사전 하나). aqr-jsonl과 달리 숫자를 모듈 안에서 aqr-num으로 코딩하지 않는 이유:

- 프로토타입에서 숫자를 앞 값과의 차로 코딩한 것이 aqr-compress가 같은 묶음을 aqr-log로 코딩한 것보다 컸다
  (aqr-xml FORMAT.md, "Design notes"의 "Prototype"). 열 코딩은 aqr-log·aqr-xsv에 이미 있고 그쪽의 개선이 그대로 온다.
- 모듈의 바이트는 아카이브마다 드는 비용이다.

다시 볼 때: 묶음이 수백 개를 넘어 스트림마다의 비용이 보일 때(그때 작은 숫자 묶음을 모듈 안에서 코딩하는 것을 잰다).

### 1.2 문맥은 두 단계

텍스트의 문맥은 (부모, 원소), 속성값의 문맥은 (원소, 속성)이다(FORMAT §4): `page/id`, `revision/id`,
`contributor/id`가 따로 모이고 `row@Id`, `row@CreationDate`도 그렇다. XMill의 기본인 경로 전체는 깊은
문서(treebank의 구문 트리)에서 묶음이 너무 많아진다.

### 1.3 골격은 텍스트 그대로

원문에서 값을 빼고 구조를 특수 바이트 0x01–0x05로 표시한다(FORMAT §5; XML 1.0 문서에는 나올 수 없어 이스케이프가
드물다). 짝이 맞는 끝 태그는 CLOSE 1바이트, 시작 태그는 OPEN 뒤에 원문 그대로이고 모듈은 OPEN과 END 뒤에서만
태그 문법을 읽는다. 주석, CDATA, 처리 명령, DOCTYPE은 그냥 바이트다. 구조를 따로 코딩하지 않는 것은 zstd가
골격을 이미 잘 압축해서다(enwik8 3.5 MB → 19 KB, DBLP 125 MB → 2.0 MB, 전체의 6%).

### 1.4 값의 구분은 LF, 아니면 길이

묶음의 값에 LF가 없으면 값마다 LF로 끝내고, 있으면 길이를 `lens`에 따로 둔다. NUL 구분은 aqr-wrt가 텍스트로
받지 않아(텍스트 아닌 바이트는 1/1000까지) DBLP가 +2.9%였고 LF로 바꾸자 −4.3%였다.

### 1.5 세그먼트

48 MiB 이상의 세그먼트(`xml:segment`; 64 MiB에서는 입력과 출력이 각각 텍스트만큼이라 호스트의 복사본까지 269 MB가
들어 응용 한도 256 MiB를 넘었다)로 자르고 세그먼트마다 적용이 하나다. 자르는 곳은 열린 원소가 네 개 이하가 되는 끝
태그나 빈 원소 태그 뒤다(한 개 이하로 했더니 행이 빈 원소인 Stack Exchange의 PostHistory 301 MB가 한 세그먼트가
되었다). 세그먼트 시작에서 열려 있는 원소의 이름은 params에, 묶음 번호는 세그먼트마다 새로 매긴다. 묶음은
세그먼트마다 스트림 하나(4 KiB 미만은 한 스트림을 나눠 씀)이고 클래스는 모두 `xml/values` 하나다. aqr-compress는
내용 스트림을 따로 다시 압축하므로 숫자 열이 다른 묶음과 섞여 aqr-xsv에 가지 않는다.

### 1.6 메모리 계획

params의 `containers`(묶음 수)와 `depth`(스택 한계), 입력·출력의 길이를 따른다. 묶음을 찾는 표는 묶음 수로 크기가
정해지는 해시 표다(`plans_hold`, `docs/arena-design.md`).

## 2. 고르기

`Format::Xml`은 텍스트로 알아본다(`detect.rs`: BOM과 공백 뒤 프롤로그를 지나 첫 원소가 오는 문서이고
`mostly_text`이며 `aqr_xml::formula::MIN_LEN` 64 KiB 이상). HTML은 뺀다(닫지 않는 `<br>`, `<img>` 때문에 끝 태그마다
엉뚱한 원소가 닫혀 값이 엉뚱한 묶음으로 간다). `.xml` 같은 이름은 따로 두지 않는다(텍스트로 알려진 파일도 같은
`table`로 간다).

`table`(`crates/aqr-compress/src/made.rs`)이 aqr-jsonl 다음, aqr-log·aqr-xsv 앞에 aqr-xml을 시도한다. 표본(256 KiB 창
8개, 창은 태그에서 자름: `xml_sample`)을 코딩해 스트림마다(컨테이너는 각각) zstd -3으로 압축한 크기를 표본 자체의
zstd -3에 견준 몫(`share`)으로 잰다.

- 텍스트 경로(aqr-wrt)보다: `xml:percent`(100%) 이하일 때만.
- aqr-log·aqr-xsv가 가져갈 파일(`table_pick`)은 그 몫의 `xml:log-percent`(135%) 이하일 때만. 줄마다 속성으로 든
  행(Stack Exchange의 `Votes`·`Badges`, OSM의 변경분)은 aqr-xml의 몫이 aqr-log의 1.44–1.85배이고 실제로 aqr-log가
  작다. 원소로 된 레코드(silesia의 stats1·tpc, treebank)는 1.01–1.04배인데도 aqr-xml이 4–19% 작다(컨테이너를 이어서
  aqr-wrt·log·xsv가 다시 압축하기 때문). 경계는 1.31배인 se-history의 `PostLinks`와 1.44배인 OSM 사이.
- 1 MiB 미만(`xml:small-len`)은 `xml:small-percent`(92%)여야 한다(고정 비용이 이득의 대부분: silesia `play3.xml`은
  0.93이었고 5.3% 더 컸다).
- 고른 뒤의 `recompress_as`는 아카이브가 원본보다 작은지(`smaller`)를 한 번 더 보지만 4 MiB 이상(`xml:uncompared`)은
  하지 않는다(표본이 이미 골랐고 enwik9에서 비교가 3/20 스레드초). `MIN_LEN` 아래는 시도하지 않는다(1 KiB로 내려도
  코퍼스의 작은 파일 27개 중 달라지는 것이 없었다).

### 2.1 컨테이너가 무엇인지 알려 주기

컨테이너 하나하나가 따로 파일이라 aqr-compress가 b2t·quopri·table(aqr-log의 표본 코딩)을 컨테이너마다 살펴보면
enwik9는 세그먼트 31개의 `text` 컨테이너로 이 살펴보기만 30배(+83%의 큰 몫)였다. 인코더가 컨테이너의 종류를 스트림의
`Kind::Words`·`Kind::Column`으로 알린다:

- `words`: 값의 평균이 40바이트 이상이고 16바이트에 공백이 하나 이상(산문): aqr-wrt로 바로(`Nest::Words`).
- `column`: 그 밖의 16 KiB 이상 컨테이너: 보통 텍스트처럼 하되 aqr-log·aqr-xsv·텍스트를 고르는 표본이 16분의 1
  (`table:column-window`).
- 16 KiB 미만은 한 스트림을 나눠 쓴다.

한 파일의 컨테이너 텍스트는 aqr-wrt 사전 하나를 나눠 쓴다(`segment_words`): 컨테이너마다 따로 둘 때보다 JMdict
−3.8%, Posts −5.5%, PubMed −6.8%, DBLP −4.1%, SwissProt −1.6%.

## 3. 남은 문제

- **컨테이너 종류별 결정 한 번**: 지금은 aqr-log·aqr-xsv 결정을 세그먼트마다 하는데, 종류별로 한 번만 하고 그
  컨테이너의 모든 세그먼트에 쓸 수 있다.
- **문맥의 깊이**: 두 단계와 세 단계, 경로 전체를 코퍼스에서 비교한다. 같은 원소 이름을 부모와 상관없이 한 묶음으로
  두는 것도(DBLP의 `inproceedings/author`와 `article/author`는 같은 사람들이다).
- **골격의 구조 코딩**: DBLP에서 골격이 6%다. 시작 태그의 속성 목록을 그 원소의 최근 목록 번호로 부르는 것(aqr-jsonl의
  키 목록처럼)을 잰다.
- **모듈 안의 숫자**: §1.1의 재검토 조건.
- **prep9에서 가져오지 않은 것**: revision id 순으로 정렬해 순서를 공짜로 얻는 것과 기여자별 주석 번호(enwik9의
  구조에 매임). IPv4를 4바이트로 쓰는 것은 묶음을 aqr-log가 받으면 주소 슬롯이 맡는다.

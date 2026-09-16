# Repository 작업 지침

## 적용 범위

이 파일은 이 Repository에서 작업하는 사람과 코딩 에이전트가 공유하는 지침의 정본이다. Codex는 `/AGENTS.md`, Claude Code는 `/.claude/CLAUDE.md`, Gemini는 `/GEMINI.md`를 진입점으로 사용하며, 세 파일은 이 파일을 가리키는 심링크다.

## 프로젝트 명세

`docs/project-spec.md`는 제품 범위, 외부 계약과 완료 조건의 정본이다. 사용자가 명시적으로 요청하지 않으면 이 문서를 수정하지 않는다.

코딩 에이전트는 프로젝트 명세의 경계 안에서 구현 구조, 제품과 개발 의존성, 테스트 도구, 품질 규칙과 추가 문서를 선택하고 변경할 수 있다. 채택한 결정은 코드와 일치하는 정본 문서에 기록하고, 같은 사실을 여러 문서에 복제하지 않는다.

## Organization 운영 기준

이 Repository에는 `ajin-scrap-monitoring/.github`의 [개발 운영 규칙](https://github.com/ajin-scrap-monitoring/.github/blob/main/GOVERNANCE.md), [기여 절차](https://github.com/ajin-scrap-monitoring/.github/blob/main/CONTRIBUTING.md)와 [보안 정책](https://github.com/ajin-scrap-monitoring/.github/blob/main/SECURITY.md)을 적용한다. GitHub Issue, 브랜치, commit, Pull Request (PR), CI(Continuous Integration), ruleset 또는 Release를 다루기 전에 최신 원문을 확인한다.

Repository별 문서는 공통 문서를 재작성하지 않으며, 이 프로젝트에만 필요한 추가 규칙이 있을 때만 같은 종류의 로컬 문서를 둔다.

## 구현 구조와 개발 순서

`docs/architecture.md`는 패키지 경계, 의존 방향과 검증 계층의 정본이다. `docs/development-plan.md`는 구현 순서, 단계별 산출물과 완료 조건의 정본이다.

`docs/sdk-compatibility.md`는 RPLIDAR SDK 이후 scan 변환과 생성 데이터 정합성 기준의 정본이다. 실제 장비 관측값은 공개 기본값이나 fixture로 복제하지 않고 `docs/internal/`의 별도 산출물로 관리한다.

`edge-platform-integration/`은 `ajin-edge-platform`의 `lidar-processing`과 공유하는 scan Proto, 합성 처리 설정과 소비자 수락 기준의 자기완결 인계 묶음이다. `contracts/lidar/v1/`은 외부 Repository에서 고정한 Proto 정본과 출처 metadata를 보관한다. 시뮬레이터는 환경 정의를 scan에 포함하지 않으며 `edge_integration` exporter가 공개 합성 환경을 `lidar-processing` 설정으로 변환한다. exporter는 합성 검증 전용이며 실제 현장 설정의 입력, 병합, 설치 보정과 적재율 계산을 책임지지 않는다.

새 작업 세션은 `docs/development-plan.md`의 현재 상태와 다음 작업을 확인한 뒤 범위를 정한다. 실제 수신 프로그램과 대상 장비의 공유 부하 검증이 필요한 작업은 문서에 남은 선행 조건을 먼저 충족한다.

시뮬레이터 구현은 Rust `src/`, 자동 검증은 `tests/`, 공개 가능한 합성 입력 예시는 `examples/`에 둔다. Python은 계약 검사와 문서 생성 자동화에만 사용하며 시뮬레이터 구현이나 OCI image에 포함하지 않는다. 지속 부하와 처리 높이는 Raspberry Pi 5에서 Release image와 `lidar-processing`을 함께 실행해 검증한다. 새로운 최상위 경계가 필요하면 코드와 함께 아키텍처 문서를 갱신한다.

Rust 외부 출력이나 생명주기를 변경하면 release profile binary를 만들고 `tools/verify_rust_runtime_contract.py`로 고정한 `lidar-processing` 계약을 직접 검증한다. 필요한 checkout, client 연결 전제와 판정 기준은 `edge-platform-integration/README.md`를 따른다.

미확정된 설정 및 전송 계약을 구현해야 하는 단계에서는 수신 프로그램과 계약을 먼저 확정한다. 공개 합성 환경으로 분류되지 않은 내부 자료의 값은 공개 기본값, 예제 또는 테스트 fixture로 사용하지 않는다.

## 공개 합성 환경

공개 합성 환경의 정본은 `examples/environment.v1.json`과 이를 참조하는 `examples/simulator.v2.json`의 조합이다. 전자는 적재 공간 치수, 경계 형상, 바닥 및 상단 높이와 센서 위치 및 방향을 정의하고, 후자는 투입구 위치와 시나리오 설정을 정의한다.

환경과 품질 설정은 `lidar_1`, `lidar_2`의 LiDAR 2대를 정확히 포함한다. 시뮬레이터와 엣지 검증은 두 센서별 독립 scan 전송 lane을 기준으로 한다.

이 정본의 공간 및 센서 규격은 실제 현장 정보에서 파생되지 않은 프로젝트용 합성 규격이다. 공개 설정, 테스트 fixture, 기술 문서, 그림과 영상에 사용할 수 있다. 다른 공개 자산은 값을 별도로 복제하지 않고 이 정본을 입력으로 사용하거나 참조한다.

공개 실행 설정의 출처와 분류는 [`docs/configuration.md`](../docs/configuration.md)가 정본이다. `measurement.sample_rate_hz`, `measurement.rotation_rate_hz`와 측정 거리 범위는 RPLIDAR S2E 기준을 반영하고, 시나리오, 측정 noise 및 distortion, 품질 분포, 전송과 진단 값은 프로젝트 합성 또는 개발 정책값으로 구분한다. 실제 센서 출력의 배열 길이는 고정하지 않으며 공개 예시의 측정점 수를 하드웨어 계약으로 해석하지 않는다.

배포 설정 계층은 CLI 인자, 환경변수, versioned JSON과 코드 기본값 순서로 적용한다. JSON은 합성 모델, 센서, 측정, 품질, seed와 관찰 복구 정책을 소유한다. 환경변수는 설정 파일과 UDS(Unix Domain Socket) 및 상태 경로, 배포 식별자, 평균 적재 주기, 수거 기준, 관찰 endpoint와 진단 출력을 소유한다. 센서 ID와 설치 형상을 환경변수에 중복하지 않는다.

시뮬레이터는 `ajin-edge-platform`의 `LidarScanSource.SubscribeScans` 계약과 같은 sensor별 server-streaming gRPC(Google Remote Procedure Call) over UDS endpoint를 제공한다. 두 endpoint는 하나의 시뮬레이터 프로세스와 적재 모델을 공유하며 sensor별 최신 frame 2개만 보관한다. 느린 구독자는 sequence gap으로 유실을 식별하고, 구독 연결 변경은 생성 모델을 초기화하지 않는다. 오류와 상태 표현은 고정한 `ajin-edge-platform` 구현을 따른다.

적재 모델 관찰 기능은 [`docs/observation.md`](../docs/observation.md)와 `contracts/observation/v1/`을 정본으로 사용한다. 시뮬레이터는 읽기 전용 적재 모델 snapshot을 기존 scan 전송과 별도 JSON Lines TCP stream으로 계속 전송한다. publisher는 최신 snapshot 1개만 보관하며 연결 실패와 느린 수신기가 생성 및 기존 scan 전송을 막지 않게 한다. 렌더링, 기록과 MP4 생성은 별도 시각화 Repository가 담당하며 시뮬레이터 image와 엣지 실행 경로에 포함하지 않는다. 별도 Repository 구현 인계는 [`docs/visualizer-requirements.md`](../docs/visualizer-requirements.md)를 따른다.

## 내부 자료

`docs/internal/`은 현장 규격, 센서 설치값, 사설 주소와 실측 데이터를 두는 로컬 전용 경로다. 이 경로의 자료는 읽기 전용 기준 자료로 취급하며, 사용자가 명시적으로 요청하지 않으면 수정, 대체, 삭제하지 않는다. 새로운 측정 결과나 보정값은 기존 파일을 덮어쓰지 않고 별도 산출물로 추가한다.

Git은 `docs/internal/README.md`를 제외한 나머지 내용을 추적하지 않는다. 실제 센서 측정값, 품질 관측 원본, 운영 로그, 사설 주소, 자격 증명과 공개 합성 환경으로 분류되지 않은 현장 정보는 공개 설정, 테스트 고정값, 로그 또는 이미지에 복제하지 않는다.

## 작업 원칙

- 변경 전에 `git status`와 관련 파일을 확인하여 기존 변경을 구분한다.
- 현재 작업 범위에 필요한 파일만 변경한다.
- 기존 작업 트리의 사용자 변경을 덮어쓰거나 되돌리지 않는다.
- 미확정된 외부 계약, 측정 의미, 인증 정책과 배포 환경을 임의로 결정하지 않는다.
- 자격 증명, 사설 주소, 현장 데이터와 공개할 수 없는 자산을 Git에 포함하지 않는다.
- 변경 범위에 맞는 정적 검사, 타입 검사, 테스트와 빌드를 실행한다.
- 작업을 마칠 때 변경 파일, 검증 명령과 결과, 남은 미확정 사항을 보고한다.

## Git 작업

- 명시적인 요청 없이 commit, push, tag, Release 또는 원격 설정 변경을 수행하지 않는다.
- 명시적인 요청 없이 branch를 전환하거나 기존 변경을 삭제하지 않는다.
- commit과 Pull Request (PR)에 코딩 에이전트가 작성했다는 메타데이터를 추가하지 않는다.
- GitHub Issue 제목과 Pull Request 제목의 summary는 명사구로 끝낸다.

## 문서 작성

- 현재 반영된 사실과 채택한 결정만 기록한다.
- 과거 수정 과정, 폐기한 대안과 작업 일지를 남기지 않는다.
- 설명 중심 문서는 서술식으로 작성하고 본문 문장은 마침표로 끝낸다.
- 목록과 표 셀은 명사구를 기본으로 사용한다.
- 약어는 처음 사용할 때 전체 이름을 함께 적는다.

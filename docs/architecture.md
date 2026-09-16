# 아키텍처

## 실행 구성 요소

실행 구성 요소는 4개다.

| 구성 요소 | 위치 | 책임 |
| --- | --- | --- |
| 합성 LiDAR 시뮬레이터 | 이 Repository의 container | 적재 모델, 센서별 scan, 관찰 stream과 상태 생성 |
| `lidar-processing` | 같은 edge 장비의 별도 container | 센서별 scan 구독, 단면 높이 계산과 융합 |
| 상태 수집기 | edge platform | 시뮬레이터 상태 파일 수집 |
| 시각화 프로그램 | 별도 개발 장비 | 관찰 stream의 3D 표시, 기록과 MP4 생성 |

시뮬레이터와 `lidar-processing`은 같은 host의 UDS(Unix Domain Socket)를 공유한다. 시뮬레이터가 센서별
gRPC(Google Remote Procedure Call) server이고 `lidar-processing`이 server-streaming 구독자다.
관찰 stream은 router를 통과할 수 있는 JSON Lines TCP 연결이며 scan 경로와 독립적이다.

```text
                       shared host directory
+------------------+  lidar_1.sock  +------------------+
| simulator        |--------------->| lidar-processing |
| shared model     |  lidar_2.sock  | gRPC subscribers |
+--------+---------+--------------->+------------------+
         |
         | JSON Lines TCP
         v
+------------------+
| visualizer       |
| remote machine   |
+------------------+
```

## 애플리케이션 경계

Rust 애플리케이션 경계는 10개다. `src/lib.rs`는 설정, 계산, 출력과 wire API를 공개하며
`scrap-monitoring-lidar-simulator` binary는 `check`, `run`과
`export-synthetic-processing-config` 명령을 제공한다.

| 경로 | 책임 |
| --- | --- |
| `src/configuration/`, `src/error.rs` | 세 JSON loader, 교차 입력 검증과 오류 분류 |
| `src/cli.rs` | CLI(Command-Line Interface), 환경변수 및 JSON override 계층과 세 명령 |
| `src/main.rs` | 명령 dispatch, process 출력과 종료 코드 |
| `src/runtime/generation.rs` | 공통 적재 모델과 sensor별 고정 계산 worker 2개의 조정 |
| `src/runtime/application.rs` | 실시간 pacing, 출력 조립, signal과 순서가 정해진 종료 생명주기 |
| `src/scan_runtime/` | 최신 frame 2개, UDS gRPC server 2개와 상태 파일 |
| `src/observation.rs` | 최신 snapshot 1개의 비차단 JSON Lines TCP publisher |
| `src/diagnostics.rs` | bounded 진단 선택, 직렬화와 파일 writer |
| `src/edge_integration/` | 공개 합성 입력의 처리 설정 변환과 결정론적 JSON 출력 |
| `src/lib.rs`의 `wire` | 고정 Proto에서 빌드 시 생성한 tonic/prost binding |

`cli`는 `configuration`과 `error`에 의존하고 `runtime/application`은 계산 runtime과 세 외부 출력
경계를 조립한다. `scan_runtime`, `observation`과 `diagnostics`는 적재 모델을 변경하지 않는다.
`build.rs`는 `contracts/lidar/v1/lidar.proto`를 잠근 compiler와 binding 생성기로 처리한다. 생성
파일은 Cargo 빌드 출력에만 두며 고정 Proto 원문을 변경하지 않는다.

`check`는 참조된 세 JSON과 모델 override를 검증하고 `check --runtime`은 UDS 경로, 상태 경로,
배포 식별자와 관찰 endpoint도 요구한다. 두 검증 모드는 socket과 상태 파일을 만들지 않는다.
`run`은 같은 설정 계층을 확인한 뒤 실제 시뮬레이션과 외부 출력을 시작한다. 설정 오류는 2,
실행 오류는 1, 정상 종료는 0으로 반환한다.

`cargo test --locked`는 Rust 설정, CLI, wire, 계산과 외부 출력 회귀 검증을 실행한다.
`cargo fmt --check`와 `cargo clippy --locked --all-targets -- -D warnings`는 Rust 정적 검증이다.
Rust live runtime의 외부 계약 검증은 release profile로 만든 binary만 사용한다.

## Rust 시뮬레이션 계산 경계

Rust 시뮬레이션 계산 경계는 6개다.

| 경로 | 책임 |
| --- | --- |
| `src/geometry/` | 유한 World 좌표 벡터, 불변 다각형, 면적과 경계 포함 판정 |
| `src/scenario/grid.rs` | 다각형 clipping, bilinear node 면적 적분, cell coverage와 이웃 쌍 |
| `src/scenario/height_field.rs` | 부피 보존 표면 갱신, 유한 반복 경사 이완과 불변 snapshot |
| `src/scenario/simulator.rs` | 적재 및 수거 plan, 절대 시각 event와 AFTER-event snapshot |
| `src/randomness.rs` | versioned seed 분리와 결정론적 난수 word 생성 |
| `src/rate_profile.rs` | bounded 구간 생성과 평균 보존 rate 계산 |

높이는 y-major 연속 배열이며 정적 grid는 `Arc`로 공유한다. `SurfaceSnapshot`은 독립된
불변 높이 배열을 보관하고 이후 표면 갱신의 영향을 받지 않는다. `HeightField`의 공개 조회는
읽기 전용 slice를 반환한다. `ScenarioSimulator`는 각 갱신 event의 상태와 불변 표면을 순서대로
반환하므로 sensor 계산은 event와 같은 시각의 sample에 갱신 후 표면을 적용할 수 있다. 광선 교차와
network 실행은 이 경계에 포함하지 않는다.

다각형과 격자는 계산 및 할당 전에 engine 상한을 검사한다. 이 값은 센서나 합성 환경의 물리 규격이 아니다.
부피 scale 계산은 포화 breakpoint 이후의 잔여 가중치 합을 사용하여 좁은 kernel의 상쇄 오차를 제한한다.
`tests/height_field.rs`는 model version 1 fixture의 모든 높이와 node 면적, 부피 및 연산 결과를
비교하고 부피 보존, 경계와 할당 상한을 검증한다.

`rate_profile`은 `randomness`의 word source만 참조하며 설정, 적재 표면과 외부 출력을 참조하지
않는다. `scenario` 계산은 `geometry`, `randomness`와 `rate_profile`을 참조하며 공개 설정을 계산
자료형으로 조립하는 adapter만 `configuration`을 참조한다. Model version, 난수 소비와 상태 event
계약은 [`simulation-model.md`](simulation-model.md)가 정본이다.

## Rust LiDAR 측정 경계

Rust LiDAR 측정 경계는 8개다.

| 경로 | 책임 |
| --- | --- |
| `src/measurement/rotation.rs` | sensor별 회전, 전역 sample index와 sample 시각 |
| `src/measurement/sdk.rs` | HQ 각도 및 거리 양자화와 driver 정수 변환 |
| `src/measurement/scene.rs` | sensor frame, 정적 장면과 동적 표면의 광선 교차 |
| `src/measurement/reference.rs` | 기준 scan, HQ 각도별 광선 및 정적 교차 cache |
| `src/measurement/snapshots.rs` | event 구간별 불변 표면 이력과 보존 경계 |
| `src/measurement/spatial.rs` | 낙하물, 빈틈과 수거 가림 event 및 거리 해석 |
| `src/measurement/generation.rs` | 반사 오류, dropout, 거리 noise와 quality 생성 |
| `src/measurement/frame.rs` | SDK 정수 변환, 완료 clock과 외부 ScanFrame 생성 |

`frame`은 `generation`과 `wire`, `generation`은 `reference`, `spatial`과 `randomness`,
`reference`는 `scene`, `snapshots`와 `scenario`, `scene`은 `geometry`와 `scenario`에 의존한다.
측정 경계는 설정 loader, network server와 runtime 생명주기를 참조하지 않는다.

`ReferenceScanner`는 실제 HQ angle tick별 광선과 정적 장면 교차 결과를 보관한다. 같은 거리에
정적 후보가 여러 개면 바닥, 입력 순서의 외벽, 고정 표면 순서에서 먼저 확인한 후보를 유지한다.
동적 표면만 표면 event snapshot에 맞춰 다시 계산한다. 기준 scan은 cache에 사용한 정적 장면과
최소 및 최대 측정 거리를 공유 metadata로 보관한다. 공간 왜곡 resolver는 이 metadata가 자신의
정적 장면 및 측정 범위와 일치하지 않으면 scan을 거부한다. Event 시각의 sample은 새 snapshot을
사용하고 coordinator는 미완료 scan 중 가장 이른 sample을 포함하는 이력까지 보존한다.

공간 왜곡 event의 수명은 `[start, end)` 구간이다. 빈틈은 기존 표면보다 먼 후보를 만들 수 있지만
바닥, 외벽이나 고정 표면을 통과시키지 않는다. 최종 거리는 공간 왜곡, 반사 오류, dropout,
절단 정규분포 noise, 0 clamp, HQ 거리 양자화, 유효 범위 판정, quality 순서로 생성한다.

`tests/rotation.rs`, `tests/scan_core.rs`, `tests/measurement_generation.rs`와
`tests/scan_frame.rs`는 model version 1 fixture와 Rust 경계 조건을 검증한다. 좌표와 광선 거리는
명시한 float tolerance로 비교하고 HQ 정수, ScanFrame field, sample 순서와 Protobuf byte fixture는
정확히 비교한다.

## 설정과 계약

설정 정본은 역할별로 분리한다.

| 경로 | 책임 |
| --- | --- |
| `contracts/environment/v1/` | 합성 공간과 센서 설치 schema |
| `contracts/v2/` | 시뮬레이터 실행 schema |
| `contracts/quality/v1/` | 합성 quality 분포 schema |
| `contracts/lidar/v1/` | `ajin-edge-platform` scan Proto와 고정 출처 |
| `contracts/observation/v1/` | 시각화 관찰 stream schema와 fixture |
| `examples/` | 공개 합성 환경과 실행 입력 정본 |
| `edge-platform-integration/` | 다른 Repository에 전달할 자기완결 통합 묶음 |

시뮬레이터의 JSON은 적재 환경, 센서 설치, 시나리오와 합성 측정만 정의한다.
`lidar-processing`은 이 JSON을 직접 읽지 않는다. `edge_integration` exporter가 센서 설치를
처리 좌표 변환, 50 mm 단면 ROI(Region of Interest)와 측정 범위로 변환하고 처리기가 요구하는
데모 calibration을 채운다. exporter는 실제 현장 설정이나 적재율 보정을 만들지 않는다. scan
frame에는 환경 정의를 포함하지 않는다.

scan 계약의 정본은 `ajin-edge-platform`의 고정 commit이다. 로컬 Proto는 출처 commit과
SHA-256으로 검증하고 Rust binding은 해당 Proto에서 빌드 시 생성한다.
`tools/verify_rust_runtime_contract.py`는 실제 Rust `run` process의 두 UDS lane에서 frame을
구독하고 같은 engine의 수락 결과와 상태, 관찰 및 종료 계약을 검사한다.

JSON schema의 `$id`는 현재 `scrap-monitoring-lidar-simulator` Repository 경로를 사용한다. 배포
환경변수와 설정 파일 이름의 호환 경계는 [`configuration.md`](configuration.md)가 정본이다.

## 적재 모델과 측정

`scenario::HeightField`는 경계 다각형 안의 node별 높이와 node 면적을 유지한다. 투입은 활성
투입구 주변에 국소 부피와 요철을 더한다. 각 갱신은 합성 안식각 35도 기준의 경사 이완을 최대
32회 수행하여 높은 node의 부피를 낮은 이웃으로 옮긴다. 수거는 전체 점유 표면을 낮추고 종료
시 빈 상태를 만든다. 같은 설정과 seed는 같은 시뮬레이션 표면을 만든다.

`measurement::SensorRotationScheduler`는 두 센서의 독립 회전과 내부 `scan_id`를 관리한다.
측정점은 sample rate와 rotation rate에서 계산한 시뮬레이션 시각에 생성한다. 한 scan의 배열
길이는 고정 계약이 아니며 회전 경계에 포함된 실제 측정점 수로 정한다. 장면은 적재 표면,
바닥, 외벽과 고정 표면 중 센서에서 가장 가까운 광선 교차를 반환하므로 허공은 거리 0, 벽과
바닥은 해당 교차 거리로 표현한다.

합성 오차는 거리 noise, 낙하물, 빈틈, 수거 가림, 반사 경로 오류, dropout과 quality 분포를
분리해 적용한다. 기준 교차 결과와 최종 측정 결과는 별도 자료형으로 유지한다.

## ScanFrame 변환

`measurement::ScanFrameFactory`는 `ajin-edge-platform` SDK(Software Development Kit) adapter의 출력 규칙을
그대로 적용한다.

| 필드 | 생성 규칙 |
| --- | --- |
| `angle_mdeg` | HQ Q14 각도를 외부 driver 정수식으로 millidegree 변환 후 안정 정렬 |
| `distance_mm` | HQ Q2 거리를 정수 나눗셈으로 millimeter 변환 |
| `quality` | 합성 8-bit HQ quality를 오른쪽으로 2 bit 이동 |
| `acquired_at_unix_ms` | scan 완료 시점의 wall clock |
| `acquired_monotonic_ns` | scan 완료 시점의 monotonic clock |
| `scan_hz` | 같은 sensor의 연속 완료 monotonic 시각 차이 |
| `sequence` | 첫 rate 측정용 scan을 건너뛴 뒤 instance별 1부터 증가 |
| `instance_id` | 시뮬레이터 process 시작마다 sensor별 새 UUID |

내부 `scan_id`와 wire `sequence`는 책임이 다르다. 내부 값은 시뮬레이션 회전 식별자이고 wire
값은 외부 driver instance의 공개 순서다. 시뮬레이터 재시작은 새 `instance_id`와 sequence 1로
시작한다.

## gRPC 출력과 상태

`scan_runtime::GrpcScanRuntime`은 하나의 process에서 정확히 2개 sensor UDS endpoint를 연다. 파일 이름은
환경 JSON의 sensor ID에서 결정한다. 각 endpoint는 최신 frame 2개만 보관하고 최대 구독자
8개를 받는다. 느린 구독자는 가장 오래된 frame을 잃을 수 있으며 다음 `sequence`의 gap으로
유실을 식별한다. 구독자 연결, 종료와 재연결은 생성과 적재 모델을 중단하거나 초기화하지 않는다.

빈 `consumer_id`와 128 byte 초과 값은 gRPC `INVALID_ARGUMENT`, 구독자 상한 초과는
`RESOURCE_EXHAUSTED`로 응답한다. 이 값과 메시지는 `ajin-edge-platform` 구현을 따른다. UDS는 시뮬레이터가
시작할 때 mode `0660`으로 만들고 종료할 때 제거한다. 기존 경로가 socket이 아니면 덮어쓰지
않고 시작에 실패한다. UDS client의 HTTP/2 authority 요구사항은
[`../edge-platform-integration/`](../edge-platform-integration/)이 정본이다.

상태 파일은 `ajin-edge-platform` 상태 schema, service 이름과 orchestrator directory 구조를 따른다. 첫
sensor는 `lidar-driver-a`, 둘째 sensor는 `lidar-driver-b`다. 각 파일은 상태 root 아래의
`<service>/<service>.json`에 있다. 시뮬레이터는 2초마다 임시 파일을 같은 하위 directory에서
원자적으로 교체한다. 첫 frame 전 상태는 `STARTING`, 게시 후 상태는 `HEALTHY`다.

## 관찰 출력

관찰 publisher는 연결마다 정적 scene header를 1회 보내고 기본 1초마다 동적 적재 모델
snapshot을 보낸다. 최신 대기 snapshot 1개만 유지하며 연결 실패와 느린 수신기는 scan 생성과
gRPC 출력을 막지 않는다. 관찰 stream은 scan 계약, `lidar-processing` 설정과 상태 파일을 변경하지
않는다. 세부 형식은 [`observation.md`](observation.md)가 정본이다.

## 생명주기와 검증

Rust `run` 명령은 입력을 검증한 뒤 공통 적재 모델, 두 센서 측정기, gRPC server, 상태 writer,
관찰 publisher와 선택적 진단 writer를 조립한다. SIGINT와 SIGTERM은 생성을 중단하고 publisher와
server를 닫은 뒤 집계를 기록한다. gRPC 구독자가 없어도 생성과 최신 frame 갱신은 계속된다.

자동 검증은 4개 계층이다.

| 경로 | 검증 범위 |
| --- | --- |
| `tests/*.rs` | Rust 설정, 계산, 출력과 생명주기 |
| `tests/contract/` | JSON schema, Proto 출처와 인계 fixture |
| `tests/fixtures/model-v1/` | 결정론적 수치 및 wire 기준값 |
| `tests/edge/` | ARM64 image의 두 UDS, 관찰과 상태 출력 |

단위 및 통합 검증은 외부 네트워크에 의존하지 않는다. Rust live runtime의 외부 구현 직접 호환
검증은 별도 고정 checkout과 미리 만든 release binary를 입력으로 사용한다. 지속 부하와 처리
높이는 실제 Raspberry Pi 5 실행 구성에서 판정한다. pixel 전체를 고정하는 시각 snapshot 검증은
이 Repository의 범위가 아니다.

## Repository 구조

```text
.github/workflows/
contracts/
  environment/v1/
  lidar/v1/
  observation/v1/
  quality/v1/
  v2/
docs/
edge-platform-integration/
examples/
Cargo.toml
Cargo.lock
build.rs
rust-toolchain.toml
src/
  lib.rs
  main.rs
  cli.rs
  diagnostics.rs
  error.rs
  observation.rs
  output_format.rs
  randomness.rs
  rate_profile.rs
  edge_integration/
  configuration/
  geometry/
  measurement/
  runtime/
  scan_runtime/
  scenario/
tests/
tools/
```

`docs/project-spec.md`와 기존 `docs/internal/**`은 읽기 전용 경계다.

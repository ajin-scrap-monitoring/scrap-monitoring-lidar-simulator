# 설정 출처와 기본 프로파일

## 설정 책임

설정 책임은 4개로 분리한다.

| 설정 | 정본 | 소비자 |
| --- | --- | --- |
| 합성 환경과 생성 모델 | `examples/`의 versioned JSON 3개 | 시뮬레이터와 processing config exporter |
| 공용 scan UDS Host 경로 | 배포 환경변수 | Docker 배포 명령 |
| 시뮬레이터 실행 | CLI(Command-Line Interface) 인자와 환경변수 | 시뮬레이터 |
| 합성 높이 처리 | exporter가 만든 외부 형식 JSON | 검증용 `lidar-processing` |

`lidar-processing`은 시뮬레이터의 환경 JSON을 직접 읽지 않는다. 시뮬레이터는 환경 정의를 scan
frame에 포함하지 않는다. exporter가 공개 합성 환경의 센서 위치와 방향을 `lidar-processing` 설정의
강체 변환과 ROI(Region of Interest)로 변환한다. 이 설정은 합성 검증 전용이다.

## 공개 합성 입력

공개 합성 실행은 다음 3개 파일의 조합이다.

| 파일 | 책임 | schema |
| --- | --- | --- |
| `examples/environment.v1.json` | 적재 공간, 바닥, 상단과 센서 설치 | `contracts/environment/v1/` |
| `examples/simulator.v2.json` | 시나리오, 측정, 표면, 관찰 복구, 진단과 seed | `contracts/v2/` |
| `examples/quality-profile.v1.json` | 센서별 합성 quality 분포 | `contracts/quality/v1/` |

공개 환경은 `lidar_1`, `lidar_2`의 LiDAR 2대를 정확히 포함한다. loader는 두 센서가 아니거나
세 파일의 sensor ID가 다르면 입력을 거부한다. 공간 치수, 센서 위치, 방향과 투입구 좌표는
이 파일에서만 관리한다.

공간 및 센서 규격은 프로젝트용 합성값이고 공개 가능하다. quality 분포, 시나리오와 오차도
결정론적 개발용 합성값이다. 실제 측정값, 품질 관측 원본, 운영 로그, 사설 주소와 자격 증명은
공개 설정에 포함하지 않는다.

## 적용 우선순위

시뮬레이터 실행값은 다음 순서로 선택한다.

| 우선순위 | 계층 | 책임 |
| --- | --- | --- |
| 1 | CLI 인자 | 현재 process의 명시적 override |
| 2 | 환경변수 | container 배포값 |
| 3 | versioned JSON | 합성 모델값 |
| 4 | 코드 기본값 | 관찰 주기 1초 |

센서 ID, 설치 형상, 측정과 합성 오차는 환경변수로 받지 않는다. 배포 식별자와 host 경로는
JSON에 넣지 않는다.

`check --runtime`은 모든 배포 설정을 검증하지만 socket이나 상태 파일을 만들지 않는다.
`run`은 검증된 설정만으로 실제 생성과 외부 출력을 시작하며 별도 비공개 fallback을 두지
않는다.

`SCRAP_LIDAR_SIMULATOR_*` 환경변수 prefix와 `simulator.v2.json` 파일 이름은 공개 배포 및 설정
식별자다.

## 배포 환경변수

`SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR`은 시뮬레이터와 `lidar-processing`이 공유하는 Host 절대 경로다. Docker
배포 명령은 같은 Host directory를 시뮬레이터의 `/run/lidar`와 처리 container의 `/sockets`에
mount한다. 공개 예시값은 `/opt/ajin/runtime/sockets/lidar-simulator`다. 시뮬레이터 process는 이
변수를 읽지 않는다.

## 시뮬레이터 환경변수

환경변수는 14개다.

| 환경변수 | CLI 인자 | 필수 여부 또는 fallback |
| --- | --- | --- |
| `SCRAP_LIDAR_SIMULATOR_CONFIG` | `--config` | 둘 중 하나 필수 |
| `SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR` | `--grpc-socket-dir` | 둘 중 하나 필수, 절대 경로 |
| `SCRAP_LIDAR_SIMULATOR_STATUS_DIR` | `--status-dir` | 둘 중 하나 필수, 절대 경로 |
| `SITE_ID` | `--site-id` | 둘 중 하나 필수 |
| `EDGE_ID` | `--edge-id` | 둘 중 하나 필수 |
| `CONFIG_REVISION` | `--config-revision` | 둘 중 하나 필수 |
| `DEPLOYMENT_REVISION` | `--deployment-revision` | 둘 중 하나 필수 |
| `SCRAP_LIDAR_SIMULATOR_MEAN_FILL_DURATION_S` | `--mean-fill-duration-s` | `scenario.mean_fill_duration_s` |
| `SCRAP_LIDAR_SIMULATOR_COLLECTION_THRESHOLD_CENTER_RATIO` | `--collection-threshold-center-ratio` | `scenario.collection_threshold_range` |
| `SCRAP_LIDAR_SIMULATOR_OBSERVATION_HOST` | `--observation-host` | 둘 중 하나 필수 |
| `SCRAP_LIDAR_SIMULATOR_OBSERVATION_PORT` | `--observation-port` | 둘 중 하나 필수 |
| `SCRAP_LIDAR_SIMULATOR_OBSERVATION_INTERVAL_S` | `--observation-interval-s` | 1초 |
| `SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_ENABLED` | `--diagnostics-enabled` | `diagnostics.enabled` |
| `SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_OUTPUT_PATH` | `--diagnostics-output-path` | `diagnostics.output_path` |

`SITE_ID`, `EDGE_ID`, `CONFIG_REVISION`은 `lidar-processing` 설정과 같아야 한다.
`DEPLOYMENT_REVISION`은 상태 snapshot에 기록한다. 식별자는 영문자 또는 숫자로 시작하고
영문자, 숫자, `_`, `.`, `-`만 사용한다. sensor ID, `EDGE_ID`와 `CONFIG_REVISION`은 외부
driver 기준 최대 64자다. `SITE_ID`와 `DEPLOYMENT_REVISION`은 최대 128자다.

`SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR`은 두 UDS 파일을 만드는 container 내부 절대 경로다.
파일 이름은 JSON sensor ID에서 계산하며 별도 환경변수로 받지 않는다. 상태 directory도
container 내부 절대 경로다. 구독 client의 UDS channel option은 시뮬레이터 설정이 아니며
[`../edge-platform-integration/`](../edge-platform-integration/)의 연결 요구사항을 따른다.

평균 적재 주기는 0보다 큰 유한한 simulation second다. 공개 기본값은 86,400초다. 수거 기준
중심값은 0.05 초과 0.95 이하이고, 회차별 범위는 중심값의 `+-0.05`다. 공개 중심값 0.90은
0.85부터 0.95 범위를 만든다.

관찰 host와 port는 별도 시각화 프로그램의 TCP 수신 endpoint다. 공개 port는 17000이고 동적
snapshot 기본 주기는 1초다. 주기는 0초 초과 86,400초 이하만 허용한다. 진단 활성값은 소문자
`true` 또는 `false`다. 상대 진단 경로는 simulator JSON directory를 기준으로 해석한다.

현재 계약에는 자격 증명이 없다. `.env`에 자격 증명을 넣지 않고 Docker secret도 구성하지
않는다. 장비별 `.env`는 실행 경로와 주소를 포함하므로 Git에 추가하지 않는다.

진단 기록은 sensor별 앞쪽 일부 scan의 기준 광선 교차, 당시 적재 표면과 시나리오 상태를
owner-only JSON Lines 파일로 보존하는 개발 검증 기능이다. `diagnostics.sample_scan_limit_per_sensor`
값은 sensor별 0개부터 16개까지이며 공개 기본값은 2개다. 이 개수까지만 기록하므로 무제한
로그가 아니다. 외부 gRPC frame, 처리 결과와 시각화 관찰
기록을 대신하지 않는다. 환경 형상을 포함할 수 있으므로 외부 전송이나 Git 추적 대상이 아니다.

## 합성 검증용 처리 설정

다음 명령은 공개 합성 환경에서 `lidar-processing` 형식의 처리 설정을 만든다.

```bash
cargo run --locked -- export-synthetic-processing-config \
  --simulator-config examples/simulator.v2.json \
  --socket-dir /sockets \
  --site-id synthetic-site \
  --edge-id synthetic-edge \
  --config-revision synthetic-r1 \
  --output processing.synthetic.json
```

출력의 sensor별 endpoint는 `unix:/sockets/<sensor_id>.sock`이다. exporter는 환경 좌표계에서
`lidar-processing` 좌표계로의 강체 변환, 50 mm 단면, 내부 경계와 측정 범위를 계산한다. 합성
calibration은 `demo: true`다. exporter는 실제 현장 설정을 입력받거나 병합하지 않으며 실제
장비의 설치 보정과 적재율 계산 설정을 만들지 않는다. 다음 세 식별자는 합성 검증의 시뮬레이터
frame과 처리 설정에서 같은 값을 사용한다.

```text
site_id
edge_id
config_revision
```

생성 결과와 소비자 수락 기준은 [`../edge-platform-integration/`](../edge-platform-integration/)이
정본이다.

## 센서 하드웨어 기준

공개 측정 설정은 RPLIDAR S2E의 기본 운용 기준을 반영한다.

| 설정 | 값 | 적용 |
| --- | --- | --- |
| `measurement.sample_rate_hz` | 32,000 | S2E sample rate |
| `measurement.rotation_rate_hz` | 10 | S2E scan rate와 600 RPM 기준 |
| `measurement.min_distance_m` | 0.05 | 90 percent 반사율 측정 하한 |
| `measurement.max_distance_m` | 30 | 90 percent 반사율 측정 상한 |

명목 측정량은 센서당 회전당 3,200 point, 초당 10 scan과 32,000 point다. 두 센서 합계는
초당 20 scan과 64,000 point다. scan 배열 길이는 wire 고정값이 아니며 scheduler가 회전
경계로 나눈 실제 측정점 수를 사용한다. 시뮬레이터 loader는 `lidar-processing`의 frame 수락 상한에
맞춰 `ceil(sample_rate_hz / rotation_rate_hz) <= 32768`을 검증한다. JSON Schema는 두 field의
비율을 표현하지 못하므로 loader가 이 교차 field 조건을 검증한다. SDK 이후 정수 변환은
[`sdk-compatibility.md`](sdk-compatibility.md)가 정본이다.

## 합성 시나리오와 측정 오차

다음 값은 센서 사양이 아니라 프로젝트 합성 정책이다.

| 설정 묶음 | 공개 값 | 의미 |
| --- | --- | --- |
| 평균 적재 시간 | 86,400초 | 24시간 기준 적재 구간 |
| 적재 시간 및 속도 배수 | 0.8-1.2, 0.5-1.5 | 회차와 회차 내부 변동 |
| 적재 속도 변화 | 300-900초 | 5-15분 구간 변동 |
| 수거 임계치 | 0.85-0.95 | 부피 비율 기반 수거 시작 |
| 수거 시간 배수 | 0.03333333333333333-0.05 | 48-72분 수거 |
| 투입구 전환 | 활성 비율 0.5, 높이 차이 0.25m, 반경 0.5m | 국소 표면 비교 |
| 표면 확산 | 안식각 35도, 갱신당 최대 32회 | 부피 보존 경사 이완 |
| 거리 noise | 표준편차 0.01m, 제한 0.03m | 평상시 합성 거리 오차 |
| 낙하물 | 초당 0.5개 후보 | 국소 거리 감소 |
| 빈틈 | 투영 면적 비율 0.03 | 국소 거리 증가 |
| 수거 가림 | 20-40초 간격 | 이동 가림 |
| 반사 경로 오류 | 확률 0.001 | 거리 감소 후보 |
| dropout | 비활성 | 선택적 장애 모델 |

상세 좌표와 범위는 공개 JSON 정본에서만 변경한다.

## 코드 내부 상한

다음 값은 사용자 설정의 암묵적 대체값이 아니라 API와 안전 경계다.

| 상한 | 값 | 책임 |
| --- | --- | --- |
| gRPC send message | 4 MiB | frame 크기 안전 상한 |
| sensor별 구독자 | 8 | 외부 driver 호환 상한 |
| sensor별 대기 frame | 2 | latest-two 유실 제한 정책 |
| gRPC UDS endpoint | UTF-8 100 byte 이하 | Unix socket 경로 안전성 |
| 관찰 대기 snapshot | 1 | latest-only 비차단 정책 |
| 관찰 JSON Lines record | 1 MiB | record 크기 안전 상한 |
| Rust 진단 대기 record | 4 | 새 record 삭제 기반 비차단 정책 |
| Rust 진단 record | 4 MiB | 개별 record 크기 안전 상한 |
| Rust 진단 파일 | 64 MiB | 실행별 파일 크기 안전 상한 |
| 상태 갱신 | 2초 | `ajin-edge-platform` 상태 snapshot 주기 |

실행 경로는 loader가 반환한 명시적 JSON과 배포 입력을 사용한다. 하드웨어 기준 또는 외부
계약이 바뀌면 공개 입력, exporter, 고정 Proto metadata와 자동 검증을 같은 변경에서 갱신한다.
`docs/project-spec.md`와 기존 `docs/internal/**`은 이 절차의 변경 대상이 아니다.

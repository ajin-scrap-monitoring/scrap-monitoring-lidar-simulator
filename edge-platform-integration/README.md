# Edge platform LiDAR 연동 계약

이 디렉토리는 시뮬레이터를 `ajin-edge-platform`의 LiDAR driver test double로 연결하는 데 필요한
계약과 배포 경계를 제공한다. 높이 계산 프로세스의 계약 정본은 `SOURCE.json`이 고정한 외부
Repository commit이다.

전달 파일은 5개다.

| 파일 | 역할 |
|---|---|
| `README.md` | 연결, 변환과 수락 기준 |
| `SOURCE.json` | 외부 계약 정본 commit과 검토한 source 경로 |
| `validation/lidar-processing-compatibility.mbox` | 고정 처리 구현의 UDS 연결을 재현하는 최소 호환 patch |
| `v1/lidar.proto` | gRPC scan 계약의 고정 사본 |
| `v1/processing.synthetic.json` | 공개 합성 환경의 완전한 처리 입력 fixture |

## 구성 요소

연동 구성 요소는 다음 4개다.

| 구성 요소 | 책임 |
|---|---|
| 시뮬레이터 | 두 센서의 합성 scan 생성과 센서별 gRPC server 제공 |
| `lidar-processing` | 두 gRPC stream 구독과 높이 계산 |
| 시뮬레이터 JSON | 합성 환경, 센서 설치, 시나리오와 측정 모델 정의 |
| 처리 JSON | 합성 센서 강체 변환, 단면 ROI, 필터와 필수 데모 calibration 정의 |

시뮬레이터는 `lidar-driver-a`와 `lidar-driver-b`의 실행 위치를 하나의 프로세스로 대체한다. 공유
적재 모델을 한 번만 계산하지만 `lidar_1.sock`과 `lidar_2.sock`을 독립적인 UDS(Unix Domain
Socket) endpoint로 제공한다. `lidar-processing`이 각 endpoint의 `SubscribeScans`를 호출한다.

```text
Simulator container                     Processing container
+-----------------------------+         +-----------------------------+
| Shared synthetic load model |         | lidar-processing            |
| lidar_1 gRPC server          |<--------| subscriber for lidar_1      |
| lidar_2 gRPC server          |<--------| subscriber for lidar_2      |
+-----------------------------+   UDS   +-----------------------------+
```

## UDS 배포 연결

`SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR`은 시뮬레이터와 `lidar-processing`이 공유하는 edge
Host 절대 경로다. 공개 배포 예시값은 `/opt/ajin/runtime/sockets/lidar-simulator`이며 배포 환경에서 변경할 수 있다.
Docker 배포 명령은 같은 값을 두 container의 bind mount 원본으로 사용한다.

```shell
SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR=/opt/ajin/runtime/sockets/lidar-simulator

docker run \
  --mount type=bind,src="$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR",dst=/run/lidar \
  simulator-image

docker run \
  --mount type=bind,src="$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR",dst=/sockets \
  processing-image
```

시뮬레이터의 container 내부 UDS directory는 `/run/lidar`이고 `lidar-processing`의 내부 UDS
directory는 `/sockets`다. 처리 설정은 `unix:/sockets/lidar_1.sock`과
`unix:/sockets/lidar_2.sock`을 endpoint로 사용한다. Host 절대 경로를 처리 JSON에 넣지 않는다.

## Scan 계약

`v1/lidar.proto`는 외부 계약의 고정 사본이다. 시뮬레이터는 SDK(Software Development Kit)의 HQ
측정값을 `ajin-edge-platform` driver와 같은 규칙으로 정규화하여 `ScanFrame`을 만든다.

| 필드 | 시뮬레이터 의미 |
|---|---|
| `schema_version` | `1.0` |
| `edge_id` | 배포 설정의 `EDGE_ID` |
| `sensor_id` | `lidar_1` 또는 `lidar_2` |
| `sequence` | 센서 instance 안에서 1부터 증가하는 완료 scan 순번 |
| `acquired_at_unix_ms` | scan 생성 완료 Unix 시각 |
| `acquired_monotonic_ns` | scan 생성 완료 단조 시각 |
| `sdk_status` | 정상 생성 frame의 `OK` |
| `scan_hz` | 직전 완료 scan과의 단조 시각 간격으로 계산한 주기 |
| `samples` | 각도 오름차순의 mm 및 SDK quality 정규화 결과 |
| `instance_id` | 시뮬레이터 재시작마다 센서별로 바뀌는 UUID |
| `config_revision` | 배포 설정의 `CONFIG_REVISION` |

각도는 HQ `angle_z_q14`를 `ajin-edge-platform` driver와 같은 정수 반올림식으로 `angle_mdeg`에 변환한다.
거리는 HQ `dist_mm_q2`를 4로 나눈 정수 mm이며 quality는 HQ byte를 오른쪽으로 2 bit 이동한
값이다. 첫 완료 scan은 실제 `scan_hz` 기준을 만들기 위해 전송하지 않는다.

각 센서 server는 최신 frame 2개만 보관한다. 느린 구독자는 오래된 frame을 받지 않으며
`sequence` 간격으로 손실을 확인한다. `consumer_id`는 UTF-8 기준 1-128 byte이며 센서별 동시
구독자는 최대 8개다.

### Python UDS client authority

Python gRPC C-core client가 `unix:` endpoint로 channel을 만들 때 UDS filesystem 경로가 HTTP/2
`:authority`로 전달될 수 있다. Tonic server가 유효한 authority로 요청을 받도록 Python client는
`grpc.default_authority`를 명시한다.

```python
grpc.aio.insecure_channel(
    f"unix:{socket_path}",
    options=(("grpc.default_authority", "localhost"),),
)
```

이 값은 simulator 환경변수나 Proto 필드가 아니라 구독 client의 channel option이다. 실제
`lidar-processing` container와 연결하기 전에 해당 client가 이 option을 적용하는지 확인한다.
`tools/verify_rust_runtime_contract.py`의 live 구독은 이 조건을 적용한다.

## 호환 검증용 처리 image

`SOURCE.json`의 `commit`은 외부 계약을 읽는 기준 commit이다. `validation_image`는 이 commit에
`validation/lidar-processing-compatibility.mbox`를 적용해 만드는 검증 전용 파생 image의 source를
고정한다. Patch는 현재 고정된 처리 구현과 UDS 연결 및 검증 실행을 재현하기 위한 호환 변경이다.
이 patch의 처리기 내부 정책은 simulator의 제품 계약이나 결함 판정 기준이 아니다.

검증 image는 simulator Release 산출물이 아니며 GHCR(GitHub Container Registry)에 게시하지
않는다. 홈서버에서 ARM64 image와 archive를 만들고 엣지에는 archive를 전달하여 load한다. 다음
명령의 simulator checkout과 외부 clone은 서로 다른 clean directory다.

```shell
SIMULATOR_ROOT="/path/to/scrap-monitoring-lidar-simulator"
PROCESSING_ROOT="/path/to/lidar-processing-validation"
BASE_COMMIT="666ca6067a3bb86833b74140cb659049025d0dae"
VALIDATION_COMMIT="55b2e9d9401682c237a42945d9f548a4c912951f"

git clone https://github.com/ajin-scrap-monitoring/ajin-edge-platform.git "$PROCESSING_ROOT"
git -C "$PROCESSING_ROOT" checkout --detach "$BASE_COMMIT"
git -C "$PROCESSING_ROOT" am --committer-date-is-author-date \
  "$SIMULATOR_ROOT/edge-platform-integration/validation/lidar-processing-compatibility.mbox"
test "$(git -C "$PROCESSING_ROOT" rev-parse HEAD)" = "$VALIDATION_COMMIT"

uv sync --directory "$PROCESSING_ROOT" --frozen --group dev
uv run --directory "$PROCESSING_ROOT" --frozen --group dev pytest
uv run --directory "$PROCESSING_ROOT" --frozen --group dev ruff check .

docker buildx build \
  --platform linux/arm64 \
  --load \
  --file "$PROCESSING_ROOT/services/lidar-processing/Dockerfile" \
  --label org.opencontainers.image.source=https://github.com/ajin-scrap-monitoring/ajin-edge-platform \
  --label org.opencontainers.image.revision="$VALIDATION_COMMIT" \
  --label org.opencontainers.image.version=0.1.0-validation-arm64 \
  --tag ajin-lidar-processing:0.1.0-validation-arm64 \
  "$PROCESSING_ROOT"

docker save \
  --output ajin-lidar-processing-0.1.0-validation-arm64.tar \
  ajin-lidar-processing:0.1.0-validation-arm64
docker image inspect \
  --format '{{.Id}}' \
  ajin-lidar-processing:0.1.0-validation-arm64
```

엣지에서는 archive를 `docker load`로 읽고 마지막 명령이 출력한 `sha256:` image ID와
`VALIDATION_COMMIT`을 연계 검증 기록에 남긴다. 원격 외부 Repository에는 branch, commit 또는
image를 게시하지 않는다.

## 환경과 처리 설정

시뮬레이터의 `examples/environment.v1.json`은 합성 환경의 정본이다. `lidar-processing`은 이
schema를 직접 읽지 않는다. 합성 통합 검증을 준비할 때 다음 exporter가 환경과 품질 설정을
`lidar-processing` JSON으로 변환한다.

```shell
cargo run --locked -- export-synthetic-processing-config \
  --simulator-config examples/simulator.v2.json \
  --socket-dir /sockets \
  --site-id synthetic-site \
  --edge-id synthetic-edge \
  --config-revision synthetic-r1 \
  --output processing.synthetic.json
```

`SITE_ID`, `EDGE_ID`, `CONFIG_REVISION`과 `SCRAP_LIDAR_SIMULATOR_CONFIG` 환경변수로 같은 값을
제공할 수 있다. 출력은 공개 합성 환경의 연동 검증만 대상으로 하며 실제 edge platform 설정을
입력받거나 병합하지 않는다. 실제 센서 설치 보정, 융합 보정과 적재율 계산은 이 계약의 범위 밖이다.

`lidar-processing`의 `CONFIG_SHA256`은 exporter가 쓴 최종 설정 파일에서 계산한다.

```shell
CONFIG_SHA256="$(sha256sum /config/processing.synthetic.json | awk '{print $1}')"
```

`v1/processing.synthetic.json`은 공개 합성 환경에서 exporter가 만든 검증 fixture다. 두 센서의
아래 방향 90도 구간 중 적재 공간 내부를 연속으로 관측하는 긴 구간을 선택하며 50 mm 단면
bin을 만든다. 이 설정의 `demo`와 `allow_demo_calibration`은 모두 `true`다. `fusion_map`은
`lidar-processing`을 실행하기 위한 항등 데모 값이며 이 Repository가 적재율 보정을 제공한다는
의미가 아니다.

## 좌표 변환 기준

시뮬레이터의 sensor 광선과 `lidar-processing` 좌표는 다음 식으로 연결한다.

```text
world_point = p0 + distance * (cos(angle) * u0 + sin(angle) * u90)
sdk_point = [distance * cos(-angle), distance * sin(-angle), 0]
section_point = rotation * sdk_point + translation
```

section x축은 환경의 `-u90`, section z축은 환경의 상단 방향이다. `rotation`과
`translation_mm`은 위 두 표현의 같은 광선 교차점이 같은 section x 및 z 좌표를 갖도록
계산한다. exporter는 바닥부터 상단까지 경계 polygon 내부에 남는 50 mm column의 연속 구간을
ROI(Region of Interest)로 선택한다. `lidar-processing`이 요구하는 비순환 angle interval 때문에
0-90도 또는 270-360도인 아래 방향 한 quadrant만 사용한다.

`bottom_mm`과 `max_height_mm`은 선택한 각 column의 바닥 및 상단 절대 z 좌표다. 길이는
`(roi_x_max - roi_x_min) / 50`과 같다. `lidar-processing`이 요구하는 `base_weight`는 두 sensor에
0.5씩 넣고 `fusion_map`은 항등 데모 값으로 넣는다. 이 값은 적재율 보정의 정확성을 주장하지
않는다. fixture가 `lidar-processing` loader와 `ProcessingEngine`에서 `GOOD` 상태와 sensor별 coverage
1.0을 만드는지는 CI가 직접 검증한다.

## 상태 계약

시뮬레이터는 상태 root에 `lidar-driver-a/lidar-driver-a.json`과
`lidar-driver-b/lidar-driver-b.json`을 기록한다. 첫 번째 환경 센서는 service
`lidar-driver-a`, 두 번째 환경 센서는 `lidar-driver-b`에 대응한다. 이 하위 경로는 `ajin-edge-platform`
orchestrator의 수집 구조와 같다. 상태 schema, 식별자, freshness, sequence, frame loss와 정상
상태 표현은 외부 driver 계약을 따른다.

## 관찰 stream 경계

TCP(Transmission Control Protocol) JSON Lines observation stream은 시각화 전용이다. 환경
형상은 최초 observation header에 포함되며 `lidar-processing`은 이 stream을 읽지 않는다.
scan gRPC 계약에는 환경 형상이나 observation record를 추가하지 않는다.

## 수락 검증

수락 검증은 고정한 외부 commit 및 Proto와 실제 Rust runtime의 직접 연결을 검사한다. 미리 만든
release profile 실행 파일을 요구하며 debug 실행 결과는 이 검증을 대신하지 않는다.

```shell
cargo build --locked --release \
  --bin scrap-monitoring-lidar-simulator
uv run --locked --group integration \
  python -m tools.verify_rust_runtime_contract \
  --runtime-binary target/release/scrap-monitoring-lidar-simulator \
  --edge-platform-root /path/to/ajin-edge-platform
```

검증기는 같은 Rust binary의 exporter로 처리 설정을 만들고 `run` process를 시작한다. 이어서
`lidar_1.sock`과 `lidar_2.sock`을 실제 gRPC client로 구독하고 받은 frame을 고정한
`ProcessingEngine`에 넣는다. 판정 항목은 sensor별 연속 sequence, SDK 정규화 범위, 두 sensor와
융합 결과의 `GOOD`, 단면 coverage 1.0, 상태 파일의 필드 집합, 식별자 및 `HEALTHY` 진행값, 관찰
TCP 전송, SIGTERM 종료 코드 0, 구독 EOF와 소유 UDS 제거다.

# 배포와 실행

이 문서의 배포 대상은 합성 LiDAR 시뮬레이터를 사용하는 개발 및 검증 환경이다. 실제 LiDAR를
사용하는 운영 환경에는 이 시뮬레이터 image와 합성 처리 설정을 배포하지 않는다.

검증 제어 장비에는 Git, Rust 1.96.0, GitHub CLI와 Docker Engine이 필요하다. 엣지 장비에는
Docker Engine만 필요하며 source checkout과 build tool을 설치하지 않는다.

## 배포 산출물

GitHub Release 첨부 파일은 2개다.

| 산출물 | 용도 |
| --- | --- |
| `edge-platform-integration-v1.tar.gz` | `ajin-edge-platform` 통합 인계 |
| `oci-image.txt` | ARM64 OCI image의 digest 고정 참조 |

OCI(Open Container Initiative) image는 Raspberry Pi 5용 `linux/arm64` 단일 실행 platform이다.
Scratch image는 정적으로 link한 Rust 실행 파일과 필수 라이선스 고지만 포함한다. 최종 image는
UID(User Identifier)와 GID(Group Identifier) 10001, read-only root filesystem 전제로 실행하며
Python, compiler, uv, SDK와 개발 의존성을 포함하지 않는다.

## Release 선택

배포자는 GitHub Release tag와 image digest를 함께 고정한다.

```bash
RELEASE_TAG="$(gh release view \
  --repo ajin-scrap-monitoring/scrap-monitoring-lidar-simulator \
  --json tagName --jq .tagName)"

WORK_DIR="$(mktemp -d)"
RELEASE_DIR="$WORK_DIR/release"
RELEASE_SOURCE="$WORK_DIR/source"
mkdir "$RELEASE_DIR"
gh release download "$RELEASE_TAG" \
  --repo ajin-scrap-monitoring/scrap-monitoring-lidar-simulator \
  --pattern oci-image.txt \
  --dir "$RELEASE_DIR"
git clone --branch "$RELEASE_TAG" --depth 1 \
  https://github.com/ajin-scrap-monitoring/scrap-monitoring-lidar-simulator.git \
  "$RELEASE_SOURCE"
EXPECTED_REVISION="$(gh api \
  "repos/ajin-scrap-monitoring/scrap-monitoring-lidar-simulator/commits/$RELEASE_TAG" \
  --jq .sha)"
test "$(git -C "$RELEASE_SOURCE" rev-parse HEAD)" = "$EXPECTED_REVISION"
IMAGE_REF="$(sed -n '1p' "$RELEASE_DIR/oci-image.txt")"
printf '%s\n' "$IMAGE_REF"
```

`latest` tag와 변경 가능한 version tag를 실행 입력으로 사용하지 않는다. `oci-image.txt`의
`name@sha256:<digest>` 값을 사용한다. Public GHCR(GitHub Container Registry) package이므로
pull credential은 필요하지 않다.

검증 제어 장비가 Release tag와 image를 선택하고 실행 환경 파일, 공개 JSON과 합성 처리 설정을
엣지에 전달한다. 엣지 장비는 Git checkout과 build tool 없이 image 실행, 설정 파일과 runtime
directory만 관리한다.

## 배포 입력

배포 입력은 7개다.

| 입력 | 제공 방식 |
| --- | --- |
| 시뮬레이터 image | Release digest |
| 공개 합성 JSON 3개 | read-only bind mount |
| 시뮬레이터 실행 설정 | 장비별 `.env` |
| 합성 처리 설정 | exporter가 만든 JSON의 read-only bind mount |
| scan UDS directory | 시뮬레이터와 `lidar-processing`의 공용 bind mount |
| 상태 directory | 시뮬레이터와 상태 수집기의 공용 bind mount |
| 진단 directory | 선택적 writable bind mount |

관찰 수신 프로그램은 별도 장비에서 TCP server를 연다. 시뮬레이터 `.env`의 관찰 host와 port가
해당 endpoint를 가리킨다. scan 통신은 같은 edge host의 UDS이므로 Docker network와 TCP
port가 필요하지 않다.

## 합성 검증용 처리 설정 준비

`lidar-processing`은 시뮬레이터의 합성 환경 JSON을 직접 읽지 않는다. 검증 제어 장비에서 Release
source의 Rust exporter를 실행해 공개 합성 환경에 대응하는 처리 JSON을 만든다. 먼저 환경
파일의 `replace-with-...` placeholder와 `visualizer.example`을 검증 환경의 값으로 바꾼다.

```bash
RUN_ENV="$RELEASE_DIR/simulator.env"
cp "$RELEASE_SOURCE/.env.example" "$RUN_ENV"
"${EDITOR:-vi}" "$RUN_ENV"

set -a
. "$RUN_ENV"
set +a

cargo run --manifest-path "$RELEASE_SOURCE/Cargo.toml" --locked --release -- \
  export-synthetic-processing-config \
  --simulator-config "$RELEASE_SOURCE/examples/simulator.v2.json" \
  --socket-dir /sockets \
  --site-id "$SITE_ID" \
  --edge-id "$EDGE_ID" \
  --config-revision "$CONFIG_REVISION" \
  --output "$RELEASE_DIR/processing.synthetic.json"
```

시뮬레이터와 처리 설정의 `SITE_ID`, `EDGE_ID`, `CONFIG_REVISION`은 같아야 한다. 출력은 공개 합성
환경의 검증에만 사용한다. exporter는 실제 현장 설정을 입력받거나 운영 보정값을 만들지 않는다.

검증 제어 장비는 다음 6개 파일을 엣지의 새 전달 directory에 복사한다.

- `RELEASE_SOURCE/examples/environment.v1.json`
- `RELEASE_SOURCE/examples/simulator.v2.json`
- `RELEASE_SOURCE/examples/quality-profile.v1.json`
- `RUN_ENV`의 `simulator.env`
- `RELEASE_DIR/processing.synthetic.json`
- `RELEASE_DIR/oci-image.txt`

## Host 준비

두 container는 UID와 GID 10001로 실행한다. 공용 socket directory는 두 process가 접근할 수
있도록 10001:10001과 mode 0770으로 준비한다. 다음 명령은 검증 제어 장비에서 선택한 tag의
공개 파일을 엣지의 임시 전달 경로에 둔 뒤 엣지 장비에서 실행한다.

```bash
CONFIG_DIR=/opt/ajin/config/lidar-simulator
PROCESSING_CONFIG_DIR=/opt/ajin/config/lidar-processing
STATUS_DIR=/opt/ajin/runtime/status
DIAGNOSTICS_DIR=/opt/ajin/runtime/diagnostics/lidar-simulator
TRANSFER_DIR=/path/to/transferred-release-source
IMAGE_REF="$(sed -n '1p' "$TRANSFER_DIR/oci-image.txt")"

set -a
. "$TRANSFER_DIR/simulator.env"
set +a
: "${SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR:?set SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR in simulator.env}"

sudo docker image pull "$IMAGE_REF"

sudo install -d -m 0755 "$CONFIG_DIR" "$PROCESSING_CONFIG_DIR"
sudo install -m 0644 \
  "$TRANSFER_DIR/examples/environment.v1.json" \
  "$TRANSFER_DIR/examples/simulator.v2.json" \
  "$TRANSFER_DIR/examples/quality-profile.v1.json" \
  "$CONFIG_DIR/"
sudo install -m 0644 \
  "$TRANSFER_DIR/processing.synthetic.json" \
  "$PROCESSING_CONFIG_DIR/processing.synthetic.json"
sudo install -d -m 0755 "$STATUS_DIR"
sudo install -d -o 10001 -g 10001 -m 0770 \
  "$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR" \
  "$STATUS_DIR/lidar-driver-a" \
  "$STATUS_DIR/lidar-driver-b" \
  "$DIAGNOSTICS_DIR"
sudo install -o root -g root -m 0600 \
  "$TRANSFER_DIR/simulator.env" /etc/scrap-monitoring-lidar-simulator.env
sudoedit /etc/scrap-monitoring-lidar-simulator.env
```

다음 container 경로는 mount 대상과 일치해야 한다.

```text
SCRAP_LIDAR_SIMULATOR_CONFIG=/config/simulator.v2.json
SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR=/run/lidar
SCRAP_LIDAR_SIMULATOR_STATUS_DIR=/status
SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_OUTPUT_PATH=/data/diagnostics
```

환경변수 전체 목록과 역할은 [`configuration.md`](configuration.md)가 정본이다. 현재 시뮬레이터
계약에는 자격 증명이 없으므로 `.env`와 Docker secret에 자격 증명을 추가하지 않는다.

최종 `processing.synthetic.json`은 검증용 `lidar-processing`에 read-only로 mount한다. 처리
container의 `CONFIG_SHA256`은 설치한 파일에서 계산한다.

```bash
CONFIG_SHA256="$(sha256sum \
  "$PROCESSING_CONFIG_DIR/processing.synthetic.json" | awk '{print $1}')"
```

## 시뮬레이터 실행

```bash
sudo docker run --detach \
  --name scrap-monitoring-lidar-simulator \
  --restart unless-stopped \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges=true \
  --init \
  --log-opt max-size=10m \
  --log-opt max-file=3 \
  --env-file /etc/scrap-monitoring-lidar-simulator.env \
  --mount type=bind,src="$CONFIG_DIR",dst=/config,readonly \
  --mount type=bind,src="$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR",dst=/run/lidar \
  --mount type=bind,src="$STATUS_DIR/lidar-driver-a",dst=/status/lidar-driver-a \
  --mount type=bind,src="$STATUS_DIR/lidar-driver-b",dst=/status/lidar-driver-b \
  --mount type=bind,src="$DIAGNOSTICS_DIR",dst=/data/diagnostics \
  "$IMAGE_REF"
```

진단을 사용하지 않으면 `SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_ENABLED=false`로 지정하고 진단
mount를 생략할 수 있다. Docker Engine의 `--cpus`와 `--memory`는 장비 검증에서 확정한 값만
적용한다. Repository는 다른 edge process와 함께 측정하기 전에 검증 자원 상한을 정하지 않는다.

`lidar-processing` container는 같은 `SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR`을 `/sockets`에 mount한다.

```bash
--mount type=bind,src="$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR",dst=/sockets
```

처리 JSON의 endpoint는 `unix:/sockets/lidar_1.sock`과 `unix:/sockets/lidar_2.sock`이다. 같은 UID와 GID를
사용하므로 시뮬레이터가 mode 0660으로 만든 socket에 연결할 수 있다.

## 생명주기와 실행 확인

시뮬레이터는 구독자가 없어도 두 sensor의 scan을 계속 만들고 최신 frame 2개를 갱신한다. 처리
container가 나중에 연결하면 연결 이후의 최신 frame부터 받는다. gRPC 구독 재연결은 적재
모델을 초기화하지 않는다. 시뮬레이터 process 재시작만 새 빈 적재 모델, 새 `instance_id`와
sequence 1을 만든다.

SIGTERM은 생성, 관찰 publisher, 상태 writer와 gRPC server를 순서대로 종료한다. 정상 종료는
UDS 파일을 제거한다. 비정상 종료로 남은 socket은 다음 시작 때 socket type인지 확인한 뒤
제거한다. 일반 파일과 directory는 덮어쓰지 않는다.

```bash
sudo docker container inspect scrap-monitoring-lidar-simulator \
  --format '{{.State.Status}} {{.State.ExitCode}} {{.Image}}'
sudo docker logs --tail 20 scrap-monitoring-lidar-simulator
sudo docker image inspect "$IMAGE_REF" --format '{{index .RepoDigests 0}}'
sudo find "$SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR" "$STATUS_DIR" -maxdepth 2 \( -type f -o -type s \)
```

정상 실행은 `lidar_1.sock`, `lidar_2.sock`,
`lidar-driver-a/lidar-driver-a.json`과 `lidar-driver-b/lidar-driver-b.json`을 만든다. 이 하위
구조는 `ajin-edge-platform` orchestrator가 읽는 경로다. driver 상태는 첫 frame 전 `STARTING`, 게시 후
`HEALTHY`다.

## 반복 가능한 image 검증

검증 도구는 digest image, 공개 설정, sensor별 gRPC 구독, 관찰 TCP 수신과 상태 파일을 격리된
container에서 확인한다.

```bash
EXPECTED_REVISION="$(gh api \
  "repos/ajin-scrap-monitoring/scrap-monitoring-lidar-simulator/commits/$RELEASE_TAG" \
  --jq .sha)"
"$RELEASE_SOURCE/tests/edge/verify-rust-image.sh" \
  "$IMAGE_REF" \
  "$RELEASE_SOURCE/examples" \
  "$EXPECTED_REVISION"
```

성공 출력은 `edge_validation=passed sensors=2`를 포함한다. 검증기는 두 sensor의 기준 교차와 최종
측정점 의미, ARM64 runtime의 UDS 2개, sensor별 `HEALTHY` 상태와 sequence 진행 및 정상 종료를
검사한다. 실제 gRPC 구독과 관찰 wire 계약은 Rust 통합 테스트와 고정 외부 구현 직접 호환 검사가
담당한다. `lidar-processing`을 포함한 지속 부하와 처리 높이 검증은
[`performance.md`](performance.md)를 따른다.

## Release workflow

Release workflow는 원격 `main` 이력에 포함된 `vMAJOR.MINOR.PATCH` tag만 처리한다. Tag version은
`Cargo.toml`과 개발 도구용 `pyproject.toml` version과 같아야 한다. Workflow는 전체 검증, 외부
source 직접 검증, integration bundle과 ARM64 image build를 수행한다. Image에는 SBOM(Software
Bill of Materials)과 provenance attestation을 포함한다.

workflow는 version tag와 `sha-<full-git-sha>` tag가 같은 manifest digest인지, 실행 platform이
`linux/arm64` 하나인지, GHCR package가 Public인지 확인한 뒤 Release를 게시한다. 게시한 tag,
image tag와 Release asset은 변경하지 않는다.

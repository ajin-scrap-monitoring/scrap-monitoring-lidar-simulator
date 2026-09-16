#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 IMAGE CONFIG_DIR EXPECTED_REVISION" >&2
  exit 2
fi

image="$1"
config_dir="$(cd "$2" && pwd -P)"
expected_revision="$3"
temporary_base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
temporary_base="$(cd "$temporary_base" && pwd -P)"
runtime_root="$(mktemp -d "$temporary_base/runtime-image.XXXXXX")"
chmod 0777 "$runtime_root"
runtime_container=""
container_suffix="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"

cleanup() {
  if [[ -n "$runtime_container" ]]; then
    docker container rm --force "$runtime_container" >/dev/null 2>&1 || true
  fi
  if [[ "$runtime_root" == "$temporary_base"/runtime-image.* ]]; then
    sudo find "$runtime_root" -depth -delete
  fi
}
trap cleanup EXIT

if [[ "$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")" != "linux/arm64" ]]; then
  echo "runtime image must be linux/arm64" >&2
  exit 1
fi
if [[ "$(docker image inspect --format '{{.Config.User}}' "$image")" != "10001:10001" ]]; then
  echo "runtime image user must be 10001:10001" >&2
  exit 1
fi
revision="$(
  docker image inspect \
    --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' \
    "$image"
)"
if [[ "$revision" != "$expected_revision" ]]; then
  echo "runtime image source revision differs from the expected commit" >&2
  exit 1
fi

install -d -m 0777 "$runtime_root/sockets" "$runtime_root/status"
docker run --rm --platform linux/arm64 \
  --mount "type=bind,src=$config_dir,dst=/config,readonly" \
  "$image" check --config /config/simulator.v2.json

runtime_container="runtime-image-$container_suffix"
docker run --detach --platform linux/arm64 \
  --name "$runtime_container" \
  --mount "type=bind,src=$config_dir,dst=/config,readonly" \
  --mount "type=bind,src=$runtime_root,dst=/runtime" \
  "$image" run \
  --config /config/simulator.v2.json \
  --grpc-socket-dir /runtime/sockets \
  --status-dir /runtime/status \
  --site-id synthetic-site \
  --edge-id synthetic-edge \
  --config-revision image-smoke \
  --deployment-revision image-smoke \
  --observation-host 127.0.0.1 \
  --observation-port 17000 \
  --diagnostics-enabled false >/dev/null

for _ in {1..200}; do
  if [[ -S "$runtime_root/sockets/lidar_1.sock" \
    && -S "$runtime_root/sockets/lidar_2.sock" \
    && -s "$runtime_root/status/lidar-driver-a/lidar-driver-a.json" \
    && -s "$runtime_root/status/lidar-driver-b/lidar-driver-b.json" ]]; then
    break
  fi
  if [[ "$(docker inspect --format '{{.State.Running}}' "$runtime_container")" != "true" ]]; then
    docker logs "$runtime_container" >&2
    exit 1
  fi
  sleep 0.1
done
test -S "$runtime_root/sockets/lidar_1.sock"
test -S "$runtime_root/sockets/lidar_2.sock"
python3 - "$runtime_root/status" <<'PY'
import json
import pathlib
import sys
import time

status_root = pathlib.Path(sys.argv[1])
expected = {
    "lidar-driver-a": "lidar_1",
    "lidar-driver-b": "lidar_2",
}


def statuses() -> dict[str, dict[str, object]]:
    return {
        service: json.loads((status_root / service / f"{service}.json").read_text())
        for service in expected
    }


deadline = time.monotonic() + 10
while True:
    first = statuses()
    if all(status.get("state") == "HEALTHY" for status in first.values()):
        break
    if time.monotonic() >= deadline:
        raise SystemExit("runtime image did not publish HEALTHY sensor status")
    time.sleep(0.1)

for service, sensor_id in expected.items():
    status = first[service]
    if (
        status.get("service") != service
        or status.get("sensor_id") != sensor_id
        or status.get("sequence", 0) < 1
        or status.get("frame_loss") != 0
        or status.get("sdk_errors") != 0
        or not status.get("instance_id")
    ):
        raise SystemExit(f"invalid runtime image status: {service}")

deadline = time.monotonic() + 10
while True:
    second = statuses()
    if all(second[service].get("sequence", 0) > first[service]["sequence"] for service in expected):
        break
    if time.monotonic() >= deadline:
        raise SystemExit("runtime image sensor sequences did not advance")
    time.sleep(0.1)
PY
docker stop --time 10 "$runtime_container" >/dev/null
test "$(docker inspect --format '{{.State.ExitCode}}' "$runtime_container")" = "0"
test ! -e "$runtime_root/sockets/lidar_1.sock"
test ! -e "$runtime_root/sockets/lidar_2.sock"

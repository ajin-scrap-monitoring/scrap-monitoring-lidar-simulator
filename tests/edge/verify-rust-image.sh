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
runtime_root="$(mktemp -d "$temporary_base/rust-image.XXXXXX")"
chmod 0777 "$runtime_root"
probe_container=""
validation_container=""
container_suffix="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"

cleanup() {
  for container in "$validation_container" "$probe_container"; do
    if [[ -n "$container" ]]; then
      docker container rm --force "$container" >/dev/null 2>&1 || true
    fi
  done
  if [[ "$runtime_root" == "$temporary_base"/rust-image.* ]]; then
    sudo find "$runtime_root" -depth -delete
  fi
}
trap cleanup EXIT

if [[ "$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")" != "linux/arm64" ]]; then
  echo "Rust image must be linux/arm64" >&2
  exit 1
fi
if [[ "$(docker image inspect --format '{{.Config.User}}' "$image")" != "10001:10001" ]]; then
  echo "Rust image user must be 10001:10001" >&2
  exit 1
fi

probe_container="$(docker container create --platform linux/arm64 "$image" --help)"
rootfs="$runtime_root/rootfs.tar"
rootfs_listing="$runtime_root/rootfs.txt"
docker container export --output "$rootfs" "$probe_container"
tar --list --file "$rootfs" > "$rootfs_listing"
grep -Fxq 'usr/local/bin/scrap-monitoring-lidar-simulator' "$rootfs_listing"
grep -Fxq \
  'usr/share/doc/scrap-monitoring-lidar-simulator/THIRD_PARTY_NOTICES.html' \
  "$rootfs_listing"
grep -Fxq \
  'usr/share/doc/scrap-monitoring-lidar-simulator/RUST_COPYRIGHT.html' \
  "$rootfs_listing"
if grep -Eq '(^|/)(python([0-9]+(\.[0-9]+)*)?|cargo|rustc|cc|gcc|g\+\+|make)$' \
  "$rootfs_listing"; then
  echo "Rust runtime contains a build or Python executable" >&2
  exit 1
fi
docker container rm "$probe_container" >/dev/null
probe_container=""

install -d -m 0777 \
  "$runtime_root/sockets" \
  "$runtime_root/status" \
  "$runtime_root/results"

docker run --rm --platform linux/arm64 \
  --mount "type=bind,src=$config_dir,dst=/config,readonly" \
  "$image" check --config /config/simulator.v2.json

validation_container="rust-image-validation-$container_suffix"
docker container create --platform linux/arm64 \
  --name "$validation_container" \
  --mount "type=bind,src=$config_dir,dst=/config,readonly" \
  --mount "type=bind,src=$runtime_root,dst=/runtime" \
  "$image" edge-validation \
  --config /config/simulator.v2.json \
  --grpc-socket-dir /runtime/sockets \
  --status-dir /runtime/status \
  --site-id synthetic-site \
  --edge-id synthetic-edge \
  --config-revision rust-image-smoke \
  --deployment-revision rust-image-smoke \
  --observation-host 127.0.0.1 \
  --observation-port 17000 \
  --diagnostics-enabled false \
  --observation-mode no-op \
  --warmup-duration-s 1 \
  --measurement-duration-s 2 \
  --max-samples 30 \
  --output /runtime/results/telemetry.json >/dev/null
docker container start --attach "$validation_container"
docker container cp "$validation_container:/runtime/results/telemetry.json" - \
  | tar --extract --to-stdout > "$runtime_root/telemetry-copy.json"

python3 - "$runtime_root/telemetry-copy.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    report = json.load(source)
if report["schema_version"] != "edge-validation-telemetry.v1":
    raise SystemExit("unexpected telemetry schema")
if report["completed_batch_count"] != report["expected_sample_count"]:
    raise SystemExit("edge validation telemetry is incomplete")
if report["expected_sensor_ids"] != ["lidar_1", "lidar_2"]:
    raise SystemExit("edge validation sensor set differs")
semantics = report.get("scan_semantics")
if not isinstance(semantics, list) or [item.get("sensor_id") for item in semantics] != [
    "lidar_1",
    "lidar_2",
]:
    raise SystemExit("edge validation scan semantics differ")
for sensor in semantics:
    reference_total = sum(
        sensor[name]
        for name in ("no_hit_count", "floor_hit_count", "wall_hit_count", "surface_hit_count")
    )
    measured_total = sensor["measured_valid_count"] + sensor["measured_invalid_count"]
    if (
        sensor["sampled_scan_count"] != 2
        or sensor["reference_sample_count"] != reference_total
        or sensor["reference_sample_count"] != measured_total
        or sensor["no_hit_count"] == 0
        or sensor["floor_hit_count"] + sensor["wall_hit_count"] == 0
        or sensor["surface_hit_count"] == 0
        or sensor["measured_valid_count"] == 0
        or sensor["measured_invalid_count"] == 0
        or sensor["measured_without_reference_count"] != 0
        or sensor["reference_hit_without_measurement_count"] != 0
        or sensor["reference_change_count"] != 1
    ):
        raise SystemExit(f"edge validation scan semantics are incomplete: {sensor['sensor_id']}")
PY
docker container rm "$validation_container" >/dev/null
validation_container=""

"$(dirname "$0")/verify-runtime-image.sh" \
  "$image" "$config_dir" "$expected_revision"

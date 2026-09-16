"""Verify the live Rust runtime against the pinned lidar-processing engine."""

import argparse
import asyncio
import hashlib
import importlib
import json
import math
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator
from contextlib import contextmanager, suppress
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import grpc

_ROOT = Path(__file__).parents[1]
_SOURCE = _ROOT / "contracts" / "lidar" / "v1" / "upstream.json"
_PROTO = _ROOT / "contracts" / "lidar" / "v1" / "lidar.proto"
_SIMULATOR_CONFIG = _ROOT / "examples" / "simulator.v2.json"
_PROCESSING_FIXTURE = _ROOT / "edge-platform-integration" / "v1" / "processing.synthetic.json"
_SENSOR_IDS = ("lidar_1", "lidar_2")
_EDGE_ID = "synthetic-edge"
_CONFIG_REVISION = "synthetic-r1"
_STATUS_FIELDS = {
    "schema_version",
    "service",
    "edge_id",
    "sensor_id",
    "config_revision",
    "instance_id",
    "started_at",
    "updated_at",
    "last_progress_at",
    "state",
    "reason_codes",
    "sequence",
    "frame_loss",
    "sdk_errors",
    "last_scan_unix_ms",
    "reported_at",
    "service_version",
    "site_id",
    "deployment_revision",
}
_DEPLOYMENT_ENVIRONMENT = (
    "SITE_ID",
    "EDGE_ID",
    "CONFIG_REVISION",
    "DEPLOYMENT_REVISION",
    "CAMERA_ID",
    "CONFIG_SHA256",
    "SCRAP_LIDAR_SIMULATOR_CONFIG",
    "SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR",
    "SCRAP_LIDAR_SIMULATOR_STATUS_DIR",
    "SCRAP_LIDAR_SIMULATOR_OBSERVATION_HOST",
    "SCRAP_LIDAR_SIMULATOR_OBSERVATION_PORT",
    "SCRAP_LIDAR_SIMULATOR_OBSERVATION_INTERVAL_S",
    "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_ENABLED",
    "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_OUTPUT_PATH",
    "SCRAP_LIDAR_SIMULATOR_MEAN_FILL_DURATION_S",
    "SCRAP_LIDAR_SIMULATOR_COLLECTION_THRESHOLD_CENTER_RATIO",
)


def _positive_seconds(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return parsed


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-binary", required=True, type=Path)
    parser.add_argument("--edge-platform-root", required=True, type=Path)
    parser.add_argument("--simulator-config", default=_SIMULATOR_CONFIG, type=Path)
    parser.add_argument("--startup-timeout-s", default=15.0, type=_positive_seconds)
    parser.add_argument("--validation-timeout-s", default=15.0, type=_positive_seconds)
    return parser


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def _verify_source(edge_root: Path) -> None:
    source = json.loads(_SOURCE.read_text(encoding="utf-8"))
    actual_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=edge_root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    _require(
        actual_commit == source["commit"],
        f"edge platform checkout is {actual_commit}, expected pinned {source['commit']}",
    )
    upstream_proto = edge_root / source["path"]
    _require(
        upstream_proto.read_bytes() == _PROTO.read_bytes(),
        "local LiDAR Proto differs from the pinned edge platform contract",
    )
    digest = hashlib.sha256(_PROTO.read_bytes()).hexdigest()
    _require(
        digest == source["sha256"],
        "local LiDAR Proto digest differs from its source metadata",
    )


def _load_upstream_modules(edge_root: Path) -> tuple[Any, Any, Any, Any]:
    sys.path.insert(0, str(edge_root / "packages" / "edge-common" / "src"))
    sys.path.insert(0, str(edge_root / "services" / "lidar-processing" / "src"))
    config_module = importlib.import_module("ajin_edge.config")
    engine_module = importlib.import_module("ajin_lidar_processing.engine")
    lidar_pb2 = importlib.import_module("ajin_edge.wire.lidar_pb2")
    lidar_pb2_grpc = importlib.import_module("ajin_edge.wire.lidar_pb2_grpc")
    return config_module.load_config, engine_module.ProcessingEngine, lidar_pb2, lidar_pb2_grpc


@contextmanager
def _hide_deployment_environment() -> Iterator[None]:
    saved = {name: os.environ.pop(name) for name in _DEPLOYMENT_ENVIRONMENT if name in os.environ}
    try:
        yield
    finally:
        os.environ.update(saved)


async def _completed_process(command: list[str]) -> tuple[int, str, str]:
    process = await asyncio.create_subprocess_exec(
        *command,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, stderr = await process.communicate()
    return process.returncode or 0, stdout.decode(errors="replace"), stderr.decode(errors="replace")


async def _export_processing_config(
    binary: Path,
    simulator_config: Path,
    socket_directory: Path,
    output: Path,
) -> dict[str, Any]:
    command = [
        str(binary),
        "export-synthetic-processing-config",
        "--simulator-config",
        str(simulator_config),
        "--socket-dir",
        str(socket_directory),
        "--site-id",
        "synthetic-site",
        "--edge-id",
        _EDGE_ID,
        "--config-revision",
        _CONFIG_REVISION,
        "--output",
        str(output),
    ]
    return_code, stdout, stderr = await _completed_process(command)
    if return_code != 0:
        raise RuntimeError(
            f"Rust processing exporter exited with {return_code}: {(stderr or stdout)[-4000:]}"
        )
    generated: dict[str, Any] = json.loads(output.read_text(encoding="utf-8"))
    fixture: dict[str, Any] = json.loads(_PROCESSING_FIXTURE.read_text(encoding="utf-8"))
    normalized = json.loads(json.dumps(generated))
    generated_sensors = normalized.get("sensors")
    _require(
        isinstance(generated_sensors, list) and len(generated_sensors) == 2,
        "Rust processing configuration must contain exactly two sensors",
    )
    fixture_endpoints = {sensor["sensor_id"]: sensor["endpoint"] for sensor in fixture["sensors"]}
    for sensor in generated_sensors:
        sensor_id = sensor.get("sensor_id")
        _require(sensor_id in _SENSOR_IDS, "Rust processing configuration has an unknown sensor")
        expected_endpoint = f"unix:{socket_directory / f'{sensor_id}.sock'}"
        _require(
            sensor.get("endpoint") == expected_endpoint,
            f"Rust processing configuration has an unexpected endpoint for {sensor_id}",
        )
        sensor["endpoint"] = fixture_endpoints[sensor_id]
    _require(
        normalized == fixture,
        "Rust processing configuration differs from the integration fixture",
    )
    return generated


def _processing_engine(load_config: Any, engine_type: Any, path: Path) -> Any:
    with _hide_deployment_environment():
        config = load_config(path)
    return engine_type(config)


@dataclass(slots=True)
class LaneState:
    sensor_id: str
    frames: int = 0
    first_sequence: int = 0
    last_sequence: int = 0
    last_monotonic_ns: int = 0
    last_unix_ms: int = 0
    instance_id: str | None = None

    def record(self, frame: Any) -> None:
        _require(frame.schema_version == "1.0", "unexpected scan schema_version")
        _require(frame.edge_id == _EDGE_ID, "scan edge_id mismatch")
        _require(frame.sensor_id == self.sensor_id, "scan sensor_id does not match UDS lane")
        _require(frame.config_revision == _CONFIG_REVISION, "scan config_revision mismatch")
        _require(frame.sdk_status == "OK", "scan sdk_status is not OK")
        _require(frame.sequence >= 1, "scan sequence must be positive")
        _require(frame.acquired_at_unix_ms >= 1, "scan Unix timestamp must be positive")
        _require(frame.acquired_monotonic_ns >= 1, "scan monotonic timestamp must be positive")
        _require(math.isfinite(frame.scan_hz) and frame.scan_hz > 0, "invalid scan_hz")
        _require(0 < len(frame.samples) <= 32_768, "invalid scan sample count")
        angles = [sample.angle_mdeg for sample in frame.samples]
        _require(angles == sorted(angles), "scan samples are not ordered by angle")
        _require(
            all(
                sample.angle_mdeg < 360_000
                and sample.distance_mm <= 100_000
                and sample.quality <= 63
                for sample in frame.samples
            ),
            "scan contains an invalid normalized sample",
        )
        _require(bool(frame.instance_id), "scan instance_id is empty")
        if self.frames == 0:
            self.first_sequence = frame.sequence
            self.instance_id = frame.instance_id
        else:
            _require(frame.instance_id == self.instance_id, "scan instance_id changed within a run")
            _require(frame.sequence == self.last_sequence + 1, "scan sequence contains a gap")
            _require(
                frame.acquired_monotonic_ns > self.last_monotonic_ns,
                "scan monotonic timestamp did not increase",
            )
            _require(
                frame.acquired_at_unix_ms > self.last_unix_ms,
                "scan Unix timestamp did not increase",
            )
        self.frames += 1
        self.last_sequence = frame.sequence
        self.last_monotonic_ns = frame.acquired_monotonic_ns
        self.last_unix_ms = frame.acquired_at_unix_ms


@dataclass(slots=True)
class ObservationSink:
    connections: int = 0
    received_bytes: int = 0
    record_types: list[str] = field(default_factory=list)
    writers: set[asyncio.StreamWriter] = field(default_factory=set)

    async def handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.connections += 1
        self.writers.add(writer)
        pending = bytearray()
        try:
            while chunk := await reader.read(65_536):
                self.received_bytes += len(chunk)
                pending.extend(chunk)
                while b"\n" in pending:
                    line, _, remainder = pending.partition(b"\n")
                    pending = bytearray(remainder)
                    document = json.loads(line)
                    record_type = document.get("type")
                    _require(isinstance(record_type, str), "observation record has no type")
                    self.record_types.append(record_type)
        finally:
            self.writers.discard(writer)
            writer.close()
            with suppress(OSError):
                await writer.wait_closed()

    async def close(self) -> None:
        writers = tuple(self.writers)
        for writer in writers:
            writer.close()
        if writers:
            await asyncio.gather(
                *(writer.wait_closed() for writer in writers), return_exceptions=True
            )


async def _read_stream(stream: asyncio.StreamReader | None) -> str:
    if stream is None:
        return ""
    return (await stream.read()).decode(errors="replace")


async def _wait_for_sockets(
    process: asyncio.subprocess.Process,
    socket_paths: tuple[Path, Path],
    timeout_s: float,
) -> None:
    deadline = asyncio.get_running_loop().time() + timeout_s
    while True:
        if process.returncode is not None:
            raise RuntimeError(f"Rust runtime exited during startup with {process.returncode}")
        ready = True
        for path in socket_paths:
            try:
                ready &= stat.S_ISSOCK(path.stat().st_mode)
            except FileNotFoundError:
                ready = False
        if ready:
            return
        if asyncio.get_running_loop().time() >= deadline:
            raise RuntimeError("Rust runtime did not create both UDS endpoints")
        await asyncio.sleep(0.02)


async def _subscribe(
    channel: Any,
    lidar_pb2: Any,
    lidar_pb2_grpc: Any,
    sensor_id: str,
    queue: asyncio.Queue[tuple[str, Any, int, int]],
) -> str:
    stub = lidar_pb2_grpc.LidarScanSourceStub(channel)
    stream = stub.SubscribeScans(
        lidar_pb2.SubscribeRequest(consumer_id=f"rust-contract-{sensor_id}")
    )
    try:
        async for frame in stream:
            queue.put_nowait((sensor_id, frame, time.monotonic_ns(), time.time_ns() // 1_000_000))
    except grpc.aio.AioRpcError as error:
        raise RuntimeError(
            f"{sensor_id} gRPC stream failed with {error.code().name}: {error.details()}"
        ) from error
    return "eof"


def _validate_measurement(measurement: Any) -> None:
    _require(measurement is not None, "lidar-processing did not produce a measurement")
    _require(measurement["quality"]["state"] == "GOOD", "fused measurement is not GOOD")
    sensors = measurement["sensors"]
    _require(len(sensors) == 2, "lidar-processing did not produce exactly two sensor results")
    _require(
        {sensor["sensor_id"] for sensor in sensors} == set(_SENSOR_IDS),
        "lidar-processing sensor result identities differ",
    )
    _require(
        all(sensor["state"] == "GOOD" for sensor in sensors),
        "a lidar-processing sensor result is not GOOD",
    )
    _require(
        all(sensor["coverage_ratio"] == 1.0 for sensor in sensors),
        "a lidar-processing sensor result does not have full coverage",
    )


def _read_status(path: Path) -> dict[str, Any]:
    document = path.read_bytes()
    _require(document.isascii(), f"driver status is not ASCII: {path}")
    _require(document.endswith(b"\n"), f"driver status has no final newline: {path}")
    parsed: dict[str, Any] = json.loads(document)
    return parsed


def _status_matches(
    document: dict[str, Any],
    sensor_id: str,
    service: str,
    lane: LaneState,
) -> bool:
    return (
        set(document) == _STATUS_FIELDS
        and document.get("schema_version") == "1.0"
        and document.get("service") == service
        and document.get("sensor_id") == sensor_id
        and document.get("site_id") == "synthetic-site"
        and document.get("edge_id") == _EDGE_ID
        and document.get("config_revision") == _CONFIG_REVISION
        and document.get("deployment_revision") == "contract-r1"
        and document.get("instance_id") == lane.instance_id
        and document.get("state") == "HEALTHY"
        and document.get("reason_codes") == []
        and document.get("sdk_errors") == 0
        and isinstance(document.get("sequence"), int)
        and document["sequence"] >= lane.last_sequence
        and isinstance(document.get("last_scan_unix_ms"), int)
        and document["last_scan_unix_ms"] >= lane.last_unix_ms
        and isinstance(document.get("service_version"), str)
        and bool(document["service_version"])
    )


async def _wait_for_statuses(
    status_directory: Path,
    lanes: dict[str, LaneState],
    process: asyncio.subprocess.Process,
    timeout_s: float,
) -> dict[str, str]:
    paths = {
        "lidar_1": status_directory / "lidar-driver-a" / "lidar-driver-a.json",
        "lidar_2": status_directory / "lidar-driver-b" / "lidar-driver-b.json",
    }
    services = {"lidar_1": "lidar-driver-a", "lidar_2": "lidar-driver-b"}
    deadline = asyncio.get_running_loop().time() + timeout_s
    while True:
        if process.returncode is not None:
            raise RuntimeError(
                f"Rust runtime exited before healthy status with {process.returncode}"
            )
        states: dict[str, str] = {}
        matched = True
        for sensor_id, path in paths.items():
            try:
                document = _read_status(path)
            except OSError, ValueError, json.JSONDecodeError:
                matched = False
                continue
            states[sensor_id] = str(document.get("state"))
            matched &= _status_matches(document, sensor_id, services[sensor_id], lanes[sensor_id])
        if matched and len(states) == 2:
            return states
        if asyncio.get_running_loop().time() >= deadline:
            raise RuntimeError("both driver status documents did not become HEALTHY")
        await asyncio.sleep(0.05)


def _runtime_environment() -> dict[str, str]:
    environment = dict(os.environ)
    for name in _DEPLOYMENT_ENVIRONMENT:
        environment.pop(name, None)
    return environment


async def _terminate_process(process: asyncio.subprocess.Process) -> None:
    if process.returncode is not None:
        return
    process.send_signal(signal.SIGTERM)
    try:
        await asyncio.wait_for(process.wait(), timeout=10)
    except TimeoutError:
        process.kill()
        await process.wait()


async def _consume_until_good(
    process: asyncio.subprocess.Process,
    queue: asyncio.Queue[tuple[str, Any, int, int]],
    lanes: dict[str, LaneState],
    engine: Any,
    subscribers: list[asyncio.Task[str]],
    timeout_s: float,
) -> Any:
    dirty: set[str] = set()
    last_measurement: Any = None
    deadline = asyncio.get_running_loop().time() + timeout_s
    while asyncio.get_running_loop().time() < deadline:
        if process.returncode is not None:
            raise RuntimeError(f"Rust runtime exited during validation with {process.returncode}")
        for subscriber in subscribers:
            if subscriber.done():
                exception = subscriber.exception()
                raise RuntimeError(
                    "Rust scan subscription stopped before validation: "
                    f"result={None if exception else subscriber.result()!r}, "
                    f"error={exception!r}"
                )
        try:
            sensor_id, frame, receive_monotonic_ns, receive_unix_ms = await asyncio.wait_for(
                queue.get(), timeout=0.5
            )
        except TimeoutError:
            continue
        lanes[sensor_id].record(frame)
        _require(
            bool(
                engine.ingest(
                    frame,
                    receive_monotonic_ns=receive_monotonic_ns,
                    receive_unix_ms=receive_unix_ms,
                )
            ),
            f"pinned lidar-processing rejected a live {sensor_id} frame",
        )
        dirty.add(sensor_id)
        if min(lane.frames for lane in lanes.values()) < 5 or dirty != set(_SENSOR_IDS):
            continue
        measurement = engine.measure(
            time.monotonic_ns(),
            time.time_ns() // 1_000_000,
            {"state": "SYNCED", "offset_ms": 0},
            measurement_id="rust-runtime-contract",
            cycle_id="rust-runtime-contract",
        )
        last_measurement = measurement
        dirty.clear()
        if measurement is not None and measurement["quality"]["state"] == "GOOD":
            _validate_measurement(measurement)
            return measurement
    raise RuntimeError(
        "live Rust frames did not produce a GOOD fused measurement: "
        f"frames={{{', '.join(f'{key!r}: {value.frames}' for key, value in lanes.items())}}}, "
        f"frame_loss={engine.frame_loss}, last_measurement={last_measurement!r}"
    )


async def _clean_shutdown(
    process: asyncio.subprocess.Process,
    subscribers: list[asyncio.Task[str]],
    socket_paths: tuple[Path, Path],
) -> None:
    process.send_signal(signal.SIGTERM)
    return_code = await asyncio.wait_for(process.wait(), timeout=10)
    _require(return_code == 0, f"Rust runtime returned {return_code} after SIGTERM")
    stream_results = await asyncio.wait_for(asyncio.gather(*subscribers), timeout=3)
    _require(stream_results == ["eof", "eof"], "gRPC streams did not end cleanly")
    _require(
        all(not path.exists() for path in socket_paths),
        "Rust runtime left an owned UDS endpoint after shutdown",
    )


def _summary_values(stdout: str) -> tuple[dict[str, str], dict[str, str], dict[str, str]]:
    lines = stdout.splitlines()
    selected = []
    for prefix in ("run_id=", "scan_stream ", "observation=active "):
        matches = [line for line in lines if line.startswith(prefix)]
        _require(len(matches) == 1, f"Rust runtime summary must contain one {prefix!r} line")
        selected.append(matches[0])
    parsed = []
    for line in selected:
        values = dict(field.split("=", 1) for field in line.split() if "=" in field)
        parsed.append(values)
    return parsed[0], parsed[1], parsed[2]


async def _verify_runtime(arguments: argparse.Namespace) -> dict[str, object]:
    binary = arguments.runtime_binary.resolve(strict=True)
    edge_root = arguments.edge_platform_root.resolve(strict=True)
    simulator_config = arguments.simulator_config.resolve(strict=True)
    _require(binary.is_file() and os.access(binary, os.X_OK), "runtime binary is not executable")
    _verify_source(edge_root)
    load_config, engine_type, lidar_pb2, lidar_pb2_grpc = _load_upstream_modules(edge_root)

    with tempfile.TemporaryDirectory(prefix="lr-contract-") as directory:
        root = Path(directory)
        socket_directory = root / "sockets"
        status_directory = root / "status"
        processing_path = root / "processing.json"
        generated = await _export_processing_config(
            binary, simulator_config, socket_directory, processing_path
        )
        engine = _processing_engine(load_config, engine_type, processing_path)
        _require(
            tuple(sensor["sensor_id"] for sensor in generated["sensors"]) == _SENSOR_IDS,
            "processing configuration sensor order differs",
        )

        sink = ObservationSink()
        observation_server = await asyncio.start_server(sink.handle, "127.0.0.1", 0)
        sockets = observation_server.sockets
        _require(bool(sockets), "observation sink did not bind a TCP socket")
        observation_port = int(sockets[0].getsockname()[1])
        command = [
            str(binary),
            "run",
            "--config",
            str(simulator_config),
            "--grpc-socket-dir",
            str(socket_directory),
            "--status-dir",
            str(status_directory),
            "--site-id",
            "synthetic-site",
            "--edge-id",
            _EDGE_ID,
            "--config-revision",
            _CONFIG_REVISION,
            "--deployment-revision",
            "contract-r1",
            "--observation-host",
            "127.0.0.1",
            "--observation-port",
            str(observation_port),
            "--observation-interval-s",
            "1",
            "--diagnostics-enabled",
            "false",
        ]
        process = await asyncio.create_subprocess_exec(
            *command,
            env=_runtime_environment(),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout_task = asyncio.create_task(_read_stream(process.stdout))
        stderr_task = asyncio.create_task(_read_stream(process.stderr))
        channels: list[Any] = []
        subscribers: list[asyncio.Task[str]] = []
        success = False
        failure: Exception | None = None
        measurement: Any = None
        states: dict[str, str] = {}
        lanes = {sensor_id: LaneState(sensor_id) for sensor_id in _SENSOR_IDS}
        try:
            socket_paths = (
                socket_directory / "lidar_1.sock",
                socket_directory / "lidar_2.sock",
            )
            await _wait_for_sockets(process, socket_paths, arguments.startup_timeout_s)
            channels = [
                grpc.aio.insecure_channel(
                    f"unix:{path}",
                    options=(
                        ("grpc.max_receive_message_length", 4 * 1024 * 1024),
                        ("grpc.default_authority", "localhost"),
                    ),
                )
                for path in socket_paths
            ]
            await asyncio.wait_for(
                asyncio.gather(*(channel.channel_ready() for channel in channels)),
                timeout=arguments.startup_timeout_s,
            )
            queue: asyncio.Queue[tuple[str, Any, int, int]] = asyncio.Queue()
            subscribers = [
                asyncio.create_task(
                    _subscribe(
                        channel,
                        lidar_pb2,
                        lidar_pb2_grpc,
                        sensor_id,
                        queue,
                    )
                )
                for channel, sensor_id in zip(channels, _SENSOR_IDS, strict=True)
            ]
            measurement = await _consume_until_good(
                process, queue, lanes, engine, subscribers, arguments.validation_timeout_s
            )
            _require(engine.frame_loss == 0, "lidar-processing detected live frame loss")
            states = await _wait_for_statuses(
                status_directory, lanes, process, arguments.startup_timeout_s
            )
            await _clean_shutdown(process, subscribers, socket_paths)
            success = True
        except Exception as error:
            failure = error
        finally:
            if not success:
                await _terminate_process(process)
            for subscriber in subscribers:
                if not subscriber.done():
                    subscriber.cancel()
            if subscribers:
                await asyncio.gather(*subscribers, return_exceptions=True)
            for channel in channels:
                await channel.close()
            observation_server.close()
            await observation_server.wait_closed()
            await sink.close()
            stdout = await stdout_task
            stderr = await stderr_task
        if failure is not None:
            raise RuntimeError(
                f"Rust runtime validation failed: {failure}\n"
                f"stdout tail:\n{stdout[-4000:]}\n"
                f"stderr tail:\n{stderr[-4000:]}"
            ) from failure
        _require(success, "Rust runtime validation stopped without a result")
        _validate_measurement(measurement)
        _require(sink.connections >= 1, "observation publisher did not connect")
        _require(sink.received_bytes > 0, "observation publisher sent no bytes")
        _require(
            "load_model_stream_header" in sink.record_types,
            "observation publisher sent no stream header",
        )
        _require(
            "load_model_observation" in sink.record_types,
            "observation publisher sent no dynamic record",
        )
        run_summary, scan_summary, observation_summary = _summary_values(stdout)
        generated_count = int(run_summary["generated"])
        published_count = int(run_summary["published"])
        _require(
            generated_count - published_count == 2, "runtime did not reserve one scan per sensor"
        )
        _require(
            int(scan_summary["published"]) == published_count,
            "runtime scan summaries disagree",
        )
        _require(int(scan_summary["frame_loss"]) == 0, "runtime reported server frame loss")
        _require(int(observation_summary["sent"]) >= 1, "runtime reported no observation output")
        return {
            "exit_code": process.returncode,
            "frames": {sensor_id: lane.frames for sensor_id, lane in lanes.items()},
            "first_sequences": {
                sensor_id: lane.first_sequence for sensor_id, lane in lanes.items()
            },
            "last_sequences": {sensor_id: lane.last_sequence for sensor_id, lane in lanes.items()},
            "instance_ids": {sensor_id: lane.instance_id for sensor_id, lane in lanes.items()},
            "measurement_state": measurement["quality"]["state"],
            "sensor_ids": [sensor["sensor_id"] for sensor in measurement["sensors"]],
            "coverage": {
                sensor["sensor_id"]: sensor["coverage_ratio"] for sensor in measurement["sensors"]
            },
            "status_states": states,
            "observation_connections": sink.connections,
            "observation_bytes": sink.received_bytes,
        }


def main() -> int:
    arguments = _parser().parse_args()
    try:
        result = asyncio.run(_verify_runtime(arguments))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, TimeoutError) as error:
        print(f"runtime contract verification failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

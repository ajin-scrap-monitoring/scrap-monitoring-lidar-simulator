"""Contract and distributable fixture validation tests."""

import hashlib
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator

_ROOT = Path(__file__).parents[2]
_ENVIRONMENT_SCHEMA = _ROOT / "contracts" / "environment" / "v1" / "environment.schema.json"
_SIMULATOR_SCHEMA = _ROOT / "contracts" / "v2" / "simulator.schema.json"
_QUALITY_SCHEMA = _ROOT / "contracts" / "quality" / "v1" / "quality-profile.schema.json"
_OBSERVATION = _ROOT / "contracts" / "observation" / "v1"
_LIDAR = _ROOT / "contracts" / "lidar" / "v1"
_HANDOFF = _ROOT / "edge-platform-integration"


def _json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def test_current_json_schemas_are_valid_draft_2020_12() -> None:
    for path in (_ENVIRONMENT_SCHEMA, _SIMULATOR_SCHEMA, _QUALITY_SCHEMA):
        Draft202012Validator.check_schema(_json(path))


def test_public_schema_ids_use_the_current_repository_path() -> None:
    for path in (
        _ENVIRONMENT_SCHEMA,
        _SIMULATOR_SCHEMA,
        _QUALITY_SCHEMA,
        _OBSERVATION / "header.schema.json",
        _OBSERVATION / "observation.schema.json",
    ):
        relative = path.relative_to(_ROOT).as_posix()
        assert _json(path)["$id"] == (
            f"https://github.com/ajin-scrap-monitoring/scrap-monitoring-lidar-simulator/{relative}"
        )


def test_environment_template_uses_simulator_deployment_names() -> None:
    values = {
        key: value
        for line in (_ROOT / ".env.example").read_text(encoding="utf-8").splitlines()
        if line and not line.startswith("#")
        for key, value in (line.split("=", 1),)
    }
    assert set(values) == {
        "SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR",
        "SCRAP_LIDAR_SIMULATOR_CONFIG",
        "SCRAP_LIDAR_SIMULATOR_MEAN_FILL_DURATION_S",
        "SCRAP_LIDAR_SIMULATOR_COLLECTION_THRESHOLD_CENTER_RATIO",
        "SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR",
        "SCRAP_LIDAR_SIMULATOR_STATUS_DIR",
        "SITE_ID",
        "EDGE_ID",
        "CONFIG_REVISION",
        "DEPLOYMENT_REVISION",
        "SCRAP_LIDAR_SIMULATOR_OBSERVATION_HOST",
        "SCRAP_LIDAR_SIMULATOR_OBSERVATION_PORT",
        "SCRAP_LIDAR_SIMULATOR_OBSERVATION_INTERVAL_S",
        "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_ENABLED",
        "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_OUTPUT_PATH",
    }
    assert values["SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_HOST_DIR"] == (
        "/opt/ajin/runtime/sockets/lidar-simulator"
    )


def test_public_examples_match_current_json_schemas() -> None:
    pairs = (
        (_ENVIRONMENT_SCHEMA, _ROOT / "examples" / "environment.v1.json"),
        (_SIMULATOR_SCHEMA, _ROOT / "examples" / "simulator.v2.json"),
        (_QUALITY_SCHEMA, _ROOT / "examples" / "quality-profile.v1.json"),
    )
    for schema_path, example_path in pairs:
        Draft202012Validator(_json(schema_path)).validate(_json(example_path))


def test_simulator_schema_enforces_the_diagnostics_sample_limit() -> None:
    validator = Draft202012Validator(_json(_SIMULATOR_SCHEMA))
    document = _json(_ROOT / "examples" / "simulator.v2.json")
    document["diagnostics"]["sample_scan_limit_per_sensor"] = 16
    assert validator.is_valid(document)
    document["diagnostics"]["sample_scan_limit_per_sensor"] = 17
    assert not validator.is_valid(document)


def test_observation_fixture_matches_contract() -> None:
    header_schema = _json(_OBSERVATION / "header.schema.json")
    observation_schema = _json(_OBSERVATION / "observation.schema.json")
    lines = (_OBSERVATION / "fixtures" / "observation.v1.jsonl").read_text().splitlines()

    assert len(lines) == 2
    Draft202012Validator(header_schema).validate(json.loads(lines[0]))
    Draft202012Validator(observation_schema).validate(json.loads(lines[1]))
    header = json.loads(lines[0])
    record = json.loads(lines[1])
    assert header["run_id"] == record["run_id"]
    assert header["scene"]["sensors"][0]["sensor_id"] == "sensor-a"
    assert record["sequence"] == 1


def test_pinned_proto_and_handoff_copy_match_source_metadata() -> None:
    source = _json(_LIDAR / "upstream.json")
    handoff_source = _json(_HANDOFF / "SOURCE.json")
    proto = (_LIDAR / "lidar.proto").read_bytes()

    assert source["commit"] == handoff_source["commit"]
    assert hashlib.sha256(proto).hexdigest() == source["sha256"]
    assert (_HANDOFF / "v1" / "lidar.proto").read_bytes() == proto
    validation = handoff_source["validation_image"]
    validation_patch = _HANDOFF / validation["patch_path"]
    assert len(validation["commit"]) == 40
    assert hashlib.sha256(validation_patch.read_bytes()).hexdigest() == validation["patch_sha256"]


def test_handoff_contains_only_current_integration_artifacts() -> None:
    assert {
        path.relative_to(_HANDOFF).as_posix() for path in _HANDOFF.rglob("*") if path.is_file()
    } == {
        "README.md",
        "SOURCE.json",
        "validation/lidar-processing-compatibility.mbox",
        "v1/lidar.proto",
        "v1/processing.synthetic.json",
    }

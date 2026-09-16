use std::{fs, path::Path};

use scrap_monitoring_lidar_simulator::{
    MAX_INLET_POSITIONS,
    configuration::{
        EnvironmentConfig, QualityProfileConfig, SimulatorConfig, load_environment,
        load_quality_profile, load_simulator_config, load_simulator_inputs, parse_environment,
        parse_quality_profile, parse_simulator_config, strict_json::parse_document,
        validate_inputs,
    },
    error::{ConfigurationError, ErrorKind},
    geometry::MAX_POLYGON_VERTICES,
};
use serde_json::{Value, json};

const SIMULATOR: &str = include_str!("../examples/simulator.v2.json");
const ENVIRONMENT: &str = include_str!("../examples/environment.v1.json");
const QUALITY: &str = include_str!("../examples/quality-profile.v1.json");

fn simulator_with(pointer: &str, value: Value) -> String {
    let mut document: Value = serde_json::from_str(SIMULATOR).unwrap();
    *document.pointer_mut(pointer).unwrap() = value;
    document.to_string()
}

fn replace_fixture_value(document: &mut Value, path: &[Value], replacement: Value) {
    let (last, parents) = path.split_last().unwrap();
    let mut current = document;
    for part in parents {
        current = match part {
            Value::String(key) => current.get_mut(key).unwrap(),
            Value::Number(index) => current.get_mut(index.as_u64().unwrap() as usize).unwrap(),
            _ => panic!("fixture path segment must be a string or integer"),
        };
    }
    match last {
        Value::String(key) => {
            current
                .as_object_mut()
                .unwrap()
                .insert(key.clone(), replacement);
        }
        Value::Number(index) => {
            current[index.as_u64().unwrap() as usize] = replacement;
        }
        _ => panic!("fixture path segment must be a string or integer"),
    }
}

fn parse_fixture(source: &str, document: &str) -> Result<(), ConfigurationError> {
    match source {
        "examples/environment.v1.json" => parse_environment(document).map(|_| ()),
        "examples/simulator.v2.json" => parse_simulator_config(
            document,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples"),
        )
        .map(|_| ()),
        "examples/quality-profile.v1.json" => parse_quality_profile(document).map(|_| ()),
        _ => panic!("unknown fixture source: {source}"),
    }
}

fn expected_error_kind(name: &str) -> ErrorKind {
    match name {
        "seed-overflow"
        | "boolean-version"
        | "diagnostics-limit-overflow"
        | "sampling-under-rotation"
        | "sampling-over-frame-limit"
        | "wrong-unit"
        | "noncanonical-quality-key" => ErrorKind::Range,
        "boolean-seed" | "fractional-seed" | "boolean-quality-frequency" | "non-object-root" => {
            ErrorKind::Type
        }
        "unknown-nested-field" => ErrorKind::UnexpectedField,
        "non-unit-frame" | "repeated-polygon-node" => ErrorKind::Geometry,
        "empty-sensors" => ErrorKind::Range,
        "duplicate-field" => ErrorKind::DuplicateField,
        "non-finite-number" => ErrorKind::Json,
        "missing-fields" => ErrorKind::MissingField,
        _ => panic!("fixture case is accepted or has no error mapping: {name}"),
    }
}

fn expected_error_path(name: &str, expected: &Value) -> String {
    if name == "duplicate-field" {
        return "$.seed".into();
    }
    expected["error"]
        .as_str()
        .and_then(|message| message.split_whitespace().next())
        .filter(|path| path.starts_with('$'))
        .unwrap_or("$")
        .into()
}

fn normalized_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn normalized_environment(config: &EnvironmentConfig) -> Value {
    json!({
        "environment_id": config.environment_id,
        "boundary_xy_m": config.boundary_xy_m,
        "floor_z_m": config.floor_z_m,
        "top_z_m": config.top_z_m,
        "sensors": config.sensors.iter().map(|sensor| json!({
            "sensor_id": sensor.sensor_id,
            "p0_m": sensor.p0_m,
            "u0": sensor.u0,
            "u90": sensor.u90,
        })).collect::<Vec<_>>(),
    })
}

fn normalized_simulator(config: &SimulatorConfig, root: &Path) -> Value {
    let scenario = &config.scenario;
    let measurement = &config.measurement;
    let distortions = &measurement.distortions;
    json!({
        "seed": config.seed,
        "environment_path": normalized_path(root, &config.environment_path),
        "quality_profile_path": normalized_path(root, &config.quality_profile_path),
        "scenario": {
            "mean_fill_duration_s": scenario.mean_fill_duration_s,
            "fill_duration_factor_range": scenario.fill_duration_factor_range,
            "fill_rate_factor_range": scenario.fill_rate_factor_range,
            "fill_rate_change_duration_s_range": scenario.fill_rate_change_duration_s_range,
            "collection_threshold_range": scenario.collection_threshold_range,
            "collection_duration_factor_range": scenario.collection_duration_factor_range,
            "collection_rate_factor_range": scenario.collection_rate_factor_range,
            "collection_rate_change_duration_s_range": scenario.collection_rate_change_duration_s_range,
            "inlet_positions_xy_m": scenario.inlet_positions_xy_m,
            "inlet_switch_activation_ratio": scenario.inlet_switch_activation_ratio,
            "inlet_switch_height_difference_m": scenario.inlet_switch_height_difference_m,
            "inlet_comparison_radius_m": scenario.inlet_comparison_radius_m,
            "surface": {
                "cell_size_m": scenario.surface.cell_size_m,
                "update_interval_s": scenario.surface.update_interval_s,
                "pile_spread_radius_m": scenario.surface.pile_spread_radius_m,
                "roughness_height_range_m": scenario.surface.roughness_height_range_m,
                "roughness_radius_range_m": scenario.surface.roughness_radius_range_m,
            },
        },
        "measurement": {
            "sample_rate_hz": measurement.sample_rate_hz,
            "rotation_rate_hz": measurement.rotation_rate_hz,
            "min_distance_m": measurement.min_distance_m,
            "max_distance_m": measurement.max_distance_m,
            "distance_noise": {
                "enabled": measurement.distance_noise.enabled,
                "standard_deviation_m": measurement.distance_noise.standard_deviation_m,
                "limit_m": measurement.distance_noise.limit_m,
            },
            "distortions": {
                "falling_material": {
                    "enabled": distortions.falling_material.enabled,
                    "event_rate_per_s": distortions.falling_material.event_rate_per_s,
                    "radius_m_range": distortions.falling_material.radius_m_range,
                    "duration_s_range": distortions.falling_material.duration_s_range,
                    "distance_reduction_m_range": distortions.falling_material.distance_reduction_m_range,
                },
                "voids": {
                    "enabled": distortions.voids.enabled,
                    "surface_area_ratio": distortions.voids.surface_area_ratio,
                    "radius_m_range": distortions.voids.radius_m_range,
                    "duration_s_range": distortions.voids.duration_s_range,
                    "cover_height_increase_m": distortions.voids.cover_height_increase_m,
                    "distance_increase_m_range": distortions.voids.distance_increase_m_range,
                },
                "collection_occlusion": {
                    "enabled": distortions.collection_occlusion.enabled,
                    "event_interval_s_range": distortions.collection_occlusion.event_interval_s_range,
                    "radius_m_range": distortions.collection_occlusion.radius_m_range,
                    "duration_s_range": distortions.collection_occlusion.duration_s_range,
                    "distance_reduction_m_range": distortions.collection_occlusion.distance_reduction_m_range,
                },
                "reflection_error": {
                    "enabled": distortions.reflection_error.enabled,
                    "probability": distortions.reflection_error.probability,
                    "distance_reduction_m_range": distortions.reflection_error.distance_reduction_m_range,
                },
                "dropout": {
                    "enabled": distortions.dropout.enabled,
                    "event_interval_s_range": distortions.dropout.event_interval_s_range,
                    "duration_s_range": distortions.dropout.duration_s_range,
                },
            },
        },
        "observation_transport": {
            "connect_timeout_s": config.observation_transport.connect_timeout_s,
            "send_timeout_s": config.observation_transport.send_timeout_s,
            "reconnect_initial_delay_s": config.observation_transport.reconnect_initial_delay_s,
            "reconnect_max_delay_s": config.observation_transport.reconnect_max_delay_s,
        },
        "diagnostics": {
            "enabled": config.diagnostics.enabled,
            "output_path": normalized_path(root, &config.diagnostics.output_path),
            "sample_scan_limit_per_sensor": config.diagnostics.sample_scan_limit_per_sensor.to_string().parse::<u64>().unwrap(),
        },
    })
}

fn normalized_quality(config: &QualityProfileConfig) -> Value {
    json!({
        "sensors": config.sensors.iter().map(|sensor| json!({
            "sensor_id": sensor.sensor_id,
            "valid_distance_frequencies": sensor.valid_distance_frequencies.to_vec(),
            "invalid_distance_frequencies": sensor.invalid_distance_frequencies.to_vec(),
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn public_inputs_match_the_model_v1_normalized_fixture() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/model-v1/configuration-cases.json")).unwrap();
    let normalized = &fixture["public_normalized"];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let actual_environment = load_environment(root.join("examples/environment.v1.json")).unwrap();
    assert_eq!(
        normalized_environment(&actual_environment),
        normalized["environment.v1.json"]
    );

    let actual_simulator = load_simulator_config(root.join("examples/simulator.v2.json")).unwrap();
    assert_eq!(
        normalized_simulator(&actual_simulator, root),
        normalized["simulator.v2.json"]
    );

    let actual_quality =
        load_quality_profile(root.join("examples/quality-profile.v1.json")).unwrap();
    assert_eq!(
        normalized_quality(&actual_quality),
        normalized["quality-profile.v1.json"]
    );
}

#[test]
fn configuration_fixture_cases_match_acceptance_kind_and_path() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/model-v1/configuration-cases.json")).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let source = case["source"].as_str().unwrap();
        let document = if let Some(document) = case.get("document") {
            document.as_str().unwrap().to_owned()
        } else {
            let mut document: Value =
                serde_json::from_str(&fs::read_to_string(root.join(source)).unwrap()).unwrap();
            replace_fixture_value(
                &mut document,
                case["replace"]["path"].as_array().unwrap(),
                case["replace"]["value"].clone(),
            );
            document.to_string()
        };
        let result = parse_fixture(source, &document);
        let accepted = case["expected"]["accepted"].as_bool().unwrap();
        assert_eq!(result.is_ok(), accepted, "acceptance mismatch for {name}");
        if !accepted {
            let error = result.unwrap_err();
            assert_eq!(
                error.kind,
                expected_error_kind(name),
                "kind mismatch for {name}"
            );
            assert_eq!(
                error.path,
                expected_error_path(name, &case["expected"]),
                "path mismatch for {name}"
            );
        }
    }
}

#[test]
fn public_inputs_load_without_a_fixed_sample_array_contract() {
    let inputs = load_simulator_inputs(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
    )
    .unwrap();
    assert_eq!(inputs.simulator.seed, 123_456_789);
    assert_eq!(
        inputs
            .environment
            .sensors
            .iter()
            .map(|sensor| sensor.sensor_id.as_str())
            .collect::<Vec<_>>(),
        ["lidar_1", "lidar_2"]
    );
    assert_eq!(inputs.simulator.measurement.sample_rate_hz, 32_000.0);
    assert_eq!(inputs.simulator.measurement.rotation_rate_hz, 10.0);
    for sensor in &inputs.quality_profile.sensors {
        assert_eq!(sensor.valid_distance_frequencies.len(), 256);
        assert!(
            sensor
                .valid_distance_frequencies
                .iter()
                .any(|frequency| *frequency > 0)
        );
    }
    let uneven = parse_simulator_config(
        &simulator_with("/measurement/sample_rate_hz", json!(32_000.5)),
        ".",
    )
    .unwrap();
    assert_eq!(uneven.measurement.sample_rate_hz, 32_000.5);
}

#[test]
fn simulator_rejects_frames_above_the_processing_contract_limit() {
    let mut document: Value = serde_json::from_str(SIMULATOR).unwrap();
    document["measurement"]["rotation_rate_hz"] = json!(1.0);
    document["measurement"]["sample_rate_hz"] = json!(32_768.0);
    assert!(parse_simulator_config(&document.to_string(), ".").is_ok());

    document["measurement"]["sample_rate_hz"] = json!(32_768.000_000_000_01);
    let error = parse_simulator_config(&document.to_string(), ".").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Range);
    assert_eq!(error.path, "$.measurement.sample_rate_hz");
}

#[test]
fn strict_json_rejects_nested_duplicates_before_they_are_lost() {
    for (document, path) in [
        (r#"{"seed":1,"seed":2}"#, "$.seed"),
        (r#"{"a":[{"key":1,"key":2}]}"#, "$.a[0].key"),
        (r#"{"a":{"x":1,"x":2},"b":0}"#, "$.a.x"),
        (r#"{"\u0061":1,"a":2}"#, "$.a"),
    ] {
        let error = parse_document(document).unwrap_err();
        assert_eq!(error.kind, ErrorKind::DuplicateField, "{error}");
        assert_eq!(error.path, path);
    }
}

#[test]
fn strict_json_rejects_non_json_numbers_and_trailing_documents() {
    for document in ["NaN", "Infinity", "-Infinity", "{}{}", "{", r#"{"a": NaN}"#] {
        assert_eq!(parse_document(document).unwrap_err().kind, ErrorKind::Json);
    }
}

#[test]
fn large_integer_and_float_types_remain_distinct() {
    for seed in [0, u64::MAX] {
        assert_eq!(
            parse_simulator_config(&simulator_with("/seed", json!(seed)), ".")
                .unwrap()
                .seed,
            seed
        );
    }
    for (seed, kind) in [
        (json!(true), ErrorKind::Type),
        (json!(1.0), ErrorKind::Type),
        (json!(-1), ErrorKind::Range),
    ] {
        let error = parse_simulator_config(&simulator_with("/seed", seed), ".").unwrap_err();
        assert_eq!(error.path, "$.seed");
        assert_eq!(error.kind, kind);
    }
    let beyond_u64 = SIMULATOR.replace("123456789", "18446744073709551616");
    let error = parse_simulator_config(&beyond_u64, ".").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Range);
    assert_eq!(error.path, "$.seed");
    assert!(parse_simulator_config(&simulator_with("/config_version", json!(2.0)), ".").is_ok());
}

#[test]
fn diagnostic_limit_enforces_the_public_resource_bound() {
    let maximum = parse_simulator_config(
        &simulator_with("/diagnostics/sample_scan_limit_per_sensor", json!(16)),
        ".",
    )
    .unwrap();
    assert_eq!(maximum.diagnostics.sample_scan_limit_per_sensor.get(), 16);
    assert!(maximum.diagnostics.sample_scan_limit_per_sensor.allows(15));
    assert!(!maximum.diagnostics.sample_scan_limit_per_sensor.allows(16));

    for invalid in [
        json!(17),
        serde_json::from_str("184467440737095516160").unwrap(),
    ] {
        let error = parse_simulator_config(
            &simulator_with("/diagnostics/sample_scan_limit_per_sensor", invalid),
            ".",
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Range);
        assert_eq!(error.path, "$.diagnostics.sample_scan_limit_per_sensor");
    }
    let zero = parse_simulator_config(
        &simulator_with("/diagnostics/sample_scan_limit_per_sensor", json!(0)),
        ".",
    )
    .unwrap();
    assert!(!zero.diagnostics.sample_scan_limit_per_sensor.allows(0));
}

#[test]
fn relative_paths_are_resolved_against_the_simulator_file() {
    let simulator = parse_simulator_config(SIMULATOR, "/input/config").unwrap();
    assert_eq!(
        simulator.environment_path,
        Path::new("/input/config/environment.v1.json")
    );
    assert_eq!(
        simulator.quality_profile_path,
        Path::new("/input/config/quality-profile.v1.json")
    );
    let absolute = parse_simulator_config(
        &simulator_with("/environment_path", json!("/shared/environment.json")),
        "/input/config",
    )
    .unwrap();
    assert_eq!(
        absolute.environment_path,
        Path::new("/shared/environment.json")
    );
}

#[test]
fn every_simulator_section_enforces_semantic_bounds() {
    for (pointer, value) in [
        ("/scenario/mean_fill_duration_s", json!(0)),
        ("/scenario/fill_duration_factor_range", json!([0.8, 1.3])),
        ("/scenario/fill_rate_factor_range", json!([1.1, 1.5])),
        ("/scenario/collection_rate_factor_range", json!([0.3, 0.7])),
        ("/scenario/collection_threshold_range", json!([0.0, 0.5])),
        ("/scenario/collection_duration_factor_range", json!([2, 1])),
        ("/scenario/inlet_positions_xy_m", json!([])),
        ("/scenario/inlet_positions_xy_m", json!([[1, 1], [1, 1]])),
        ("/scenario/inlet_switch_activation_ratio", json!(1.1)),
        ("/scenario/inlet_switch_height_difference_m", json!(-0.1)),
        ("/scenario/inlet_comparison_radius_m", json!(0)),
        ("/scenario/surface/cell_size_m", json!(false)),
        ("/scenario/surface/update_interval_s", json!(-1)),
        ("/measurement/sample_rate_hz", json!(1)),
        ("/measurement/rotation_rate_hz", json!(0)),
        ("/measurement/min_distance_m", json!(0.01)),
        ("/measurement/max_distance_m", json!(31)),
        (
            "/measurement/distance_noise/standard_deviation_m",
            json!(-1),
        ),
        (
            "/measurement/distortions/falling_material/event_rate_per_s",
            json!(-1),
        ),
        (
            "/measurement/distortions/voids/surface_area_ratio",
            json!(2),
        ),
        (
            "/measurement/distortions/collection_occlusion/duration_s_range",
            json!([0, 1]),
        ),
        (
            "/measurement/distortions/reflection_error/probability",
            json!(2),
        ),
        (
            "/measurement/distortions/dropout/event_interval_s_range",
            json!([2, 1]),
        ),
        ("/observation_transport/connect_timeout_s", json!(0)),
        (
            "/observation_transport/reconnect_initial_delay_s",
            json!(100_000),
        ),
        ("/diagnostics/enabled", json!(1)),
        ("/diagnostics/output_path", json!("")),
    ] {
        assert!(
            parse_simulator_config(&simulator_with(pointer, value), ".").is_err(),
            "accepted {pointer}"
        );
    }
    let excessive_inlets = Value::Array(
        (0..=MAX_INLET_POSITIONS)
            .map(|index| json!([index, 0]))
            .collect(),
    );
    let error = parse_simulator_config(
        &simulator_with("/scenario/inlet_positions_xy_m", excessive_inlets),
        ".",
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Range);
    assert_eq!(error.path, "$.scenario.inlet_positions_xy_m");
    let overflow = SIMULATOR.replace("86400", "1e999");
    let error = parse_simulator_config(&overflow, ".").unwrap_err();
    assert_eq!(error.path, "$.scenario.mean_fill_duration_s");
    assert_eq!(error.kind, ErrorKind::Range);
}

#[test]
fn exact_fields_are_required_in_all_input_documents() {
    let mut simulator: Value = serde_json::from_str(SIMULATOR).unwrap();
    simulator["measurement"]
        .as_object_mut()
        .unwrap()
        .remove("sample_rate_hz");
    let error = parse_simulator_config(&simulator.to_string(), ".").unwrap_err();
    assert_eq!(error.kind, ErrorKind::MissingField);
    assert_eq!(error.path, "$.measurement");
    let mut environment: Value = serde_json::from_str(ENVIRONMENT).unwrap();
    environment["private"] = json!(1);
    assert_eq!(
        parse_environment(&environment.to_string())
            .unwrap_err()
            .kind,
        ErrorKind::UnexpectedField
    );
    let mut profile: Value = serde_json::from_str(QUALITY).unwrap();
    profile["sensors"][0]["extra"] = json!(1);
    assert_eq!(
        parse_quality_profile(&profile.to_string())
            .unwrap_err()
            .kind,
        ErrorKind::UnexpectedField
    );
}

#[test]
fn environment_rejects_invalid_polygons_and_sensor_frames() {
    let source: Value = serde_json::from_str(ENVIRONMENT).unwrap();
    for boundary in [
        json!([[0, 0], [1, 1]]),
        json!([[0, 0], [1, 0], [0, 0]]),
        json!([[0, 0], [1, 1], [2, 2]]),
        json!([[0, 0], [3, 3], [0, 3], [2, 0]]),
    ] {
        let mut document = source.clone();
        document["boundary_xy_m"] = boundary;
        assert!(parse_environment(&document.to_string()).is_err());
    }
    for (pointer, value) in [
        ("/sensors/0/u0", json!([0, 0, -2])),
        (
            "/sensors/0/u0",
            json!([
                -0.42695967831104137,
                -0.8924300043484943,
                -0.14586336221299234
            ]),
        ),
        ("/sensors/0/u90", source["sensors"][0]["u0"].clone()),
        (
            "/sensors/1/sensor_id",
            source["sensors"][0]["sensor_id"].clone(),
        ),
        ("/top_z_m", source["floor_z_m"].clone()),
    ] {
        let mut document = source.clone();
        *document.pointer_mut(pointer).unwrap() = value;
        assert!(
            parse_environment(&document.to_string()).is_err(),
            "accepted {pointer}"
        );
    }
    let mut non_orthogonal = source.clone();
    non_orthogonal["sensors"][0]["u0"] = json!([
        -0.8856872981832026,
        0.1494960875638922,
        -0.43955537721660043
    ]);
    non_orthogonal["sensors"][0]["u90"] = json!([
        0.46377921364958347,
        0.24085706589655095,
        -0.8525823800638144
    ]);
    assert!(parse_environment(&non_orthogonal.to_string()).is_err());
    let mut clockwise = source.clone();
    clockwise["boundary_xy_m"].as_array_mut().unwrap().reverse();
    assert!(parse_environment(&clockwise.to_string()).is_ok());

    let mut excessive = source.clone();
    excessive["boundary_xy_m"] = Value::Array(
        (0..=MAX_POLYGON_VERTICES)
            .map(|index| json!([index, 0]))
            .collect(),
    );
    let error = parse_environment(&excessive.to_string()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Range);
    assert_eq!(error.path, "$.boundary_xy_m");
}

#[test]
fn quality_keys_counts_and_input_sensor_sets_are_strict() {
    for frequency in [
        json!({}),
        json!({"01": 1}),
        json!({"256": 1}),
        json!({"+1": 1}),
        json!({"1": 0}),
        json!({"1": 1.0}),
        json!({"1": true}),
        json!({"1": u64::MAX}),
    ] {
        let mut document: Value = serde_json::from_str(QUALITY).unwrap();
        document["sensors"][0]["valid_distance_frequencies"] = frequency;
        assert!(parse_quality_profile(&document.to_string()).is_err());
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json");
    let inputs = load_simulator_inputs(&path).unwrap();
    let mut changed = inputs.clone();
    changed.environment.sensors.pop();
    assert_eq!(
        validate_inputs(&changed).unwrap_err().kind,
        ErrorKind::CrossInput
    );
    let mut changed = inputs.clone();
    changed.quality_profile.sensors[0].sensor_id = "different".into();
    assert_eq!(
        validate_inputs(&changed).unwrap_err().kind,
        ErrorKind::CrossInput
    );
    let mut changed = inputs;
    changed.simulator.scenario.inlet_positions_xy_m = vec![[1_000_000.0, 1_000_000.0]];
    assert_eq!(
        validate_inputs(&changed).unwrap_err().kind,
        ErrorKind::CrossInput
    );
}

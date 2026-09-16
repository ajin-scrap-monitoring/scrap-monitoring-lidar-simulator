use std::{fs, path::Path, process::Command};

use scrap_monitoring_lidar_simulator::{
    configuration::{SimulatorInputs, load_simulator_inputs},
    edge_integration::{build_synthetic_processing_config, write_synthetic_processing_config},
};
use serde_json::Value;

fn inputs() -> SimulatorInputs {
    load_simulator_inputs(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"))
        .unwrap()
}

fn config(inputs: &SimulatorInputs) -> Value {
    build_synthetic_processing_config(
        inputs,
        Path::new("/sockets"),
        "synthetic-site",
        "synthetic-edge",
        "synthetic-r1",
    )
    .unwrap()
}

#[test]
fn public_environment_matches_the_processing_contract() {
    let output = config(&inputs());
    let keys: Vec<_> = output
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "allow_demo_calibration",
            "calibration",
            "config_revision",
            "edge_id",
            "processing",
            "schema_version",
            "sensors",
            "site_id",
        ]
    );
    assert!(output.get("deployment_revision").is_none());
    assert_eq!(output["schema_version"], "1.0");
    assert_eq!(output["calibration"]["version"], "synthetic-scrap-pit-v1");
    assert_eq!(output["calibration"]["demo"], true);
    assert_eq!(
        output["calibration"]["fusion_map"],
        serde_json::json!([[0.0, 0.0], [1.0, 1.0]])
    );
    assert_eq!(
        output["calibration"]["single_sensor_maps"],
        serde_json::json!({})
    );
    assert_eq!(output["processing"]["target_scan_hz"], 10.0);

    let sensors = output["sensors"].as_array().unwrap();
    assert_eq!(sensors.len(), 2);
    assert_eq!(sensors[0]["sensor_id"], "lidar_1");
    assert_eq!(sensors[1]["sensor_id"], "lidar_2");
    assert_eq!(sensors[0]["endpoint"], "unix:/sockets/lidar_1.sock");
    assert_eq!(sensors[1]["endpoint"], "unix:/sockets/lidar_2.sock");
    assert_eq!(
        sensors[0]["sample_filter"]["angle_interval_mdeg"],
        serde_json::json!([270_000, 360_000])
    );
    assert_eq!(
        sensors[1]["sample_filter"]["angle_interval_mdeg"],
        serde_json::json!([0, 90_000])
    );
    assert_eq!(
        sensors[0]["calibration"]["roi_x_mm"],
        serde_json::json!([500, 4_000])
    );
    assert_eq!(
        sensors[1]["calibration"]["roi_x_mm"],
        serde_json::json!([0, 3_900])
    );
    assert_eq!(
        sensors[0]["calibration"]["bottom_mm"]
            .as_array()
            .unwrap()
            .len(),
        70
    );
    assert_eq!(
        sensors[1]["calibration"]["bottom_mm"]
            .as_array()
            .unwrap()
            .len(),
        78
    );
}

#[test]
fn transform_preserves_sdk_rays_in_the_processing_section() {
    let inputs = inputs();
    let output = config(&inputs);
    let output_sensors = output["sensors"].as_array().unwrap();
    for (sensor, processing) in inputs.environment.sensors.iter().zip(output_sensors) {
        let rotation = matrix(&processing["calibration"]["rotation"]);
        let translation = vector(&processing["calibration"]["translation_mm"]);
        let axis_x = sensor.u90.map(|value| -value);
        let axis_z = [0.0, 0.0, 1.0];
        let axis_y = cross(axis_z, axis_x);
        let section_axes = [axis_x, axis_y, axis_z];
        let origin_mm = sensor.p0_m.map(|value| value * 1_000.0);
        for angle_deg in [0.0_f64, 30.0, 90.0, 180.0, 270.0, 330.0] {
            let angle = angle_deg.to_radians();
            let distance_mm = 1_234.5;
            let world_direction = add(
                scale(sensor.u0, angle.cos()),
                scale(sensor.u90, angle.sin()),
            );
            let world_point = add(origin_mm, scale(world_direction, distance_mm));
            let sdk_point = [
                distance_mm * (-angle).cos(),
                distance_mm * (-angle).sin(),
                0.0,
            ];
            let transformed = add(matrix_vector(rotation, sdk_point), translation);
            let expected = matrix_vector(section_axes, world_point);
            for (actual, expected) in transformed.into_iter().zip(expected) {
                assert!((actual - expected).abs() <= 1e-9, "{actual} != {expected}");
            }
        }
    }
}

#[test]
fn section_selection_supports_a_non_grid_aligned_sensor_origin() {
    let mut inputs = inputs();
    inputs.environment.boundary_xy_m = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 5.0], [0.0, 5.0]];
    let sensor = &mut inputs.environment.sensors[0];
    sensor.p0_m = [0.003, 2.5, 10.0];
    sensor.u0 = [0.0, 0.0, -1.0];
    sensor.u90 = [-1.0, 0.0, 0.0];

    let output = config(&inputs);
    let first = &output["sensors"][0];
    assert_eq!(
        first["sample_filter"]["angle_interval_mdeg"],
        serde_json::json!([270_000, 360_000])
    );
    assert_eq!(
        first["calibration"]["roi_x_mm"],
        serde_json::json!([0, 4_000])
    );
}

#[test]
fn builder_rejects_invalid_identity_path_endpoint_and_sensor_geometry() {
    let baseline = inputs();
    for (directory, site, message) in [
        ("relative", "site", "absolute non-root"),
        ("/", "site", "absolute non-root"),
        ("/sockets", "bad/site", "safe deployment"),
    ] {
        let error = build_synthetic_processing_config(
            &baseline,
            Path::new(directory),
            site,
            "edge",
            "revision",
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    let overlong_edge = "x".repeat(65);
    let error = build_synthetic_processing_config(
        &baseline,
        Path::new("/sockets"),
        "site",
        &overlong_edge,
        "revision",
    )
    .unwrap_err();
    assert!(error.to_string().contains("driver-compatible"), "{error}");

    let long_directory = format!("/{}", "x".repeat(100));
    let error = build_synthetic_processing_config(
        &baseline,
        Path::new(&long_directory),
        "site",
        "edge",
        "revision",
    )
    .unwrap_err();
    assert!(error.to_string().contains("endpoint exceeds 100 bytes"));

    let mut invalid = baseline.clone();
    invalid.environment.sensors[0].u90[2] = 0.01;
    assert!(
        build_synthetic_processing_config(
            &invalid,
            Path::new("/sockets"),
            "site",
            "edge",
            "revision"
        )
        .unwrap_err()
        .to_string()
        .contains("u90 must be horizontal")
    );

    let mut oversized = baseline;
    oversized.environment.boundary_xy_m =
        vec![[0.0, 0.0], [20_000.0, 0.0], [20_000.0, 5.0], [0.0, 5.0]];
    let error = build_synthetic_processing_config(
        &oversized,
        Path::new("/sockets"),
        "site",
        "edge",
        "revision",
    )
    .unwrap_err();
    assert!(error.to_string().contains("section profile exceeds"));

    let mut overflowing = inputs();
    overflowing.environment.sensors[0].p0_m[0] = f64::MAX;
    let error = build_synthetic_processing_config(
        &overflowing,
        Path::new("/sockets"),
        "site",
        "edge",
        "revision",
    )
    .unwrap_err();
    assert!(error.to_string().contains("finite translation"));
}

#[test]
fn writer_is_deterministic_and_cli_uses_only_processing_identity_values() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!(
        "lidar-exporter-test-{}-{nonce}",
        std::process::id(),
    ));
    let first = temporary.join("first/processing.json");
    let second = temporary.join("second/processing.json");
    let output = config(&inputs());
    write_synthetic_processing_config(&first, &output).unwrap();
    write_synthetic_processing_config(&second, &output).unwrap();
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    assert!(fs::read(&first).unwrap().ends_with(b"\n"));

    let binary = env!("CARGO_BIN_EXE_scrap-monitoring-lidar-simulator");
    let cli_output = temporary.join("cli/processing.json");
    let result = Command::new(binary)
        .env_clear()
        .env(
            "SCRAP_LIDAR_SIMULATOR_CONFIG",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .env("SITE_ID", "synthetic-site")
        .env("EDGE_ID", "synthetic-edge")
        .env("CONFIG_REVISION", "synthetic-r1")
        .env("DEPLOYMENT_REVISION", "must-not-be-exported")
        .args([
            "export-synthetic-processing-config",
            "--output",
            cli_output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let exported: Value = serde_json::from_slice(&fs::read(&cli_output).unwrap()).unwrap();
    assert_eq!(exported, output);
    assert!(exported.get("deployment_revision").is_none());

    let empty = Command::new(binary)
        .env_clear()
        .env(
            "SCRAP_LIDAR_SIMULATOR_CONFIG",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .env("SITE_ID", "fallback-site")
        .env("EDGE_ID", "synthetic-edge")
        .env("CONFIG_REVISION", "synthetic-r1")
        .args([
            "export-synthetic-processing-config",
            "--output",
            temporary.join("empty.json").to_str().unwrap(),
            "--site-id",
            "",
        ])
        .output()
        .unwrap();
    assert_eq!(empty.status.code(), Some(2));
    assert!(!temporary.join("empty.json").exists());

    fs::remove_dir_all(temporary).unwrap();
}

fn vector(value: &Value) -> [f64; 3] {
    let values = value.as_array().unwrap();
    std::array::from_fn(|index| values[index].as_f64().unwrap())
}

fn matrix(value: &Value) -> [[f64; 3]; 3] {
    let values = value.as_array().unwrap();
    std::array::from_fn(|index| vector(&values[index]))
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|index| left[index] + right[index])
}

fn scale(value: [f64; 3], scalar: f64) -> [f64; 3] {
    value.map(|component| component * scalar)
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn matrix_vector(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

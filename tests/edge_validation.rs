use std::process::Command;

#[cfg(feature = "edge-validation")]
use std::{io::Read, net::TcpListener, path::Path, thread};

#[cfg(feature = "edge-validation")]
use serde_json::Value;
#[cfg(feature = "edge-validation")]
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_scrap-monitoring-lidar-simulator");

#[cfg(not(feature = "edge-validation"))]
#[test]
fn default_binary_does_not_expose_the_validation_boundary() {
    let output = Command::new(BINARY)
        .env_clear()
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("edge-validation"));
    assert!(!help.contains("no-op"));
}

#[cfg(feature = "edge-validation")]
fn command(directory: &TempDir, observation_mode: &str, observation_port: u16) -> Command {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .arg("edge-validation")
        .arg("--config")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"))
        .arg("--grpc-socket-dir")
        .arg(directory.path().join("sockets"))
        .arg("--status-dir")
        .arg(directory.path().join("status"))
        .args([
            "--site-id",
            "synthetic-site",
            "--edge-id",
            "synthetic-edge",
            "--config-revision",
            "synthetic-r1",
            "--deployment-revision",
            "edge-validation-r1",
            "--observation-host",
            "127.0.0.1",
            "--observation-port",
        ])
        .arg(observation_port.to_string())
        .args([
            "--diagnostics-enabled",
            "false",
            "--observation-mode",
            observation_mode,
            "--warmup-duration-s",
            "1",
            "--measurement-duration-s",
            "1",
            "--max-samples",
            "16",
            "--output",
        ])
        .arg(directory.path().join("telemetry.json"));
    command
}

#[cfg(feature = "edge-validation")]
fn report(directory: &TempDir) -> Value {
    serde_json::from_slice(&std::fs::read(directory.path().join("telemetry.json")).unwrap())
        .unwrap()
}

#[cfg(feature = "edge-validation")]
fn monotonic_time_ns() -> u64 {
    let current = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(current.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(current.tv_nsec).unwrap()
}

#[cfg(feature = "edge-validation")]
fn assert_common_report(report: &Value, observation_mode: &str) {
    assert_eq!(report["schema_version"], "edge-validation-telemetry.v1");
    assert_eq!(report["observation_mode"], observation_mode);
    assert_eq!(report["warmup_duration_s"], 1);
    assert_eq!(report["measurement_duration_s"], 1);
    assert_eq!(report["sample_capacity"], 16);
    assert_eq!(report["expected_sample_count"], 10);
    assert_eq!(report["completed_batch_count"], 10);
    assert_eq!(report["generated_scans"], 42);
    assert_eq!(report["dropped_sample_count"], 0);
    assert_eq!(report["overflowed"], false);
    assert_eq!(report["scenario"]["window_started_elapsed_s"], 0.0);
    assert_eq!(report["scenario"]["window_ended_elapsed_s"], 2.0);
    assert_eq!(report["scenario"]["observed_through_elapsed_s"], 2.0);
    assert_eq!(report["scenario"]["initial_state"]["phase"], "filling");
    assert_eq!(report["scenario"]["initial_state"]["cycle_index"], 0);
    assert_eq!(report["scenario"]["final_state"]["phase"], "filling");
    assert_eq!(report["scenario"]["final_state"]["cycle_index"], 0);
    assert_eq!(report["scenario"]["transition_capacity"], 64);
    assert_eq!(report["scenario"]["transition_count"], 0);
    assert_eq!(report["scenario"]["overflowed"], false);
    assert!(
        report["scenario"]["transitions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(report["batch_max_samples"].as_array().unwrap().len(), 10);
    for sensor in report["sensors"].as_array().unwrap() {
        assert_eq!(sensor["sample_count"], 10);
        assert_eq!(sensor["missing_sample_count"], 0);
        assert_eq!(sensor["first_sequence"], 10);
        assert_eq!(sensor["last_sequence"], 19);
        for sample in sensor["samples"].as_array().unwrap() {
            let deadline = sample["deadline_monotonic_ns"].as_u64().unwrap();
            let published = sample["published_monotonic_ns"].as_u64().unwrap();
            assert_eq!(sample["latency_ns"].as_u64().unwrap(), published - deadline);
        }
    }
    for sensor in report["scan_stream"].as_array().unwrap() {
        assert_eq!(sensor["published_frames"], 20);
        assert_eq!(sensor["frame_loss"], 0);
    }
    let semantic_evidence = report["scan_semantics"].as_array().unwrap();
    assert_eq!(semantic_evidence.len(), 2);
    for sensor in semantic_evidence {
        assert_eq!(sensor["sampled_scan_count"], 1);
        assert_eq!(sensor["reference_sample_count"], 3_200);
        let no_hit = sensor["no_hit_count"].as_u64().unwrap();
        let floor = sensor["floor_hit_count"].as_u64().unwrap();
        let wall = sensor["wall_hit_count"].as_u64().unwrap();
        let surface = sensor["surface_hit_count"].as_u64().unwrap();
        let measured_valid = sensor["measured_valid_count"].as_u64().unwrap();
        let measured_invalid = sensor["measured_invalid_count"].as_u64().unwrap();
        assert_eq!(no_hit + floor + wall + surface, 3_200);
        assert_eq!(measured_valid + measured_invalid, 3_200);
        assert!(no_hit > 0);
        assert!(floor + wall > 0);
        assert!(surface > 0);
        assert_eq!(sensor["measured_without_reference_count"], 0);
        assert_eq!(sensor["reference_hit_without_measurement_count"], 0);
        assert_eq!(sensor["reference_change_count"], 0);
    }
}

#[cfg(feature = "edge-validation")]
#[test]
fn no_op_mode_completes_a_bounded_run_without_observation_work() {
    let directory = TempDir::new().unwrap();
    let output = command(&directory, "no-op", 9).output().unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = report(&directory);
    assert_common_report(&report, "no-op");
    assert_eq!(report["observation"]["accepted_records"], 0);
    assert_eq!(report["observation"]["sent_records"], 0);
    assert_eq!(report["observation"]["dropped_records"], 0);
    assert_eq!(report["observation"]["connection_failures"], 0);
}

#[cfg(feature = "edge-validation")]
#[test]
fn actual_mode_streams_observations_in_the_same_feature_build() {
    let directory = TempDir::new().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let receiver = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    });

    let output = command(&directory, "actual", port).output().unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let received = receiver.join().unwrap();
    let records = received
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let (header, observations) = records.split_first().unwrap();
    assert_eq!(header["type"], "load_model_stream_header");
    let report = report(&directory);
    assert_common_report(&report, "actual");
    assert!(report["observation"]["accepted_records"].as_u64().unwrap() >= 2);
    assert!(report["observation"]["sent_records"].as_u64().unwrap() >= 2);
    assert_eq!(
        report["observation"]["sent_records"].as_u64().unwrap(),
        u64::try_from(observations.len()).unwrap()
    );
    assert!(
        observations
            .iter()
            .all(|record| record["type"] == "load_model_observation")
    );
    assert_eq!(report["observation"]["dropped_records"], 0);
    assert_eq!(report["observation"]["connection_failures"], 0);
}

#[cfg(feature = "edge-validation")]
#[test]
fn validation_arguments_reject_zero_measurement_and_relative_output() {
    for (extra, expected) in [
        (
            vec!["--warmup-duration-s", "0"],
            "must be a positive integer",
        ),
        (
            vec!["--measurement-duration-s", "0"],
            "must be a positive integer",
        ),
        (
            vec!["--output", "relative.json"],
            "must be an absolute path",
        ),
        (
            vec!["--max-samples", "100001"],
            "must be an integer from 1 through 100000",
        ),
    ] {
        let directory = TempDir::new().unwrap();
        let output = command(&directory, "no-op", 9)
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }
}

#[cfg(feature = "edge-validation")]
#[test]
fn elapsed_explicit_start_fails_without_creating_a_result() {
    let directory = TempDir::new().unwrap();
    let output = command(&directory, "no-op", 9)
        .args(["--start-at-monotonic-ns", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("precedes setup completion"));
    assert!(!directory.path().join("telemetry.json").exists());
    assert!(!directory.path().join("sockets/lidar_1.sock").exists());
    assert!(!directory.path().join("sockets/lidar_2.sock").exists());
}

#[cfg(feature = "edge-validation")]
#[test]
fn future_explicit_start_anchors_the_reported_measurement_window() {
    let directory = TempDir::new().unwrap();
    let start_at = monotonic_time_ns() + 2_000_000_000;
    let output = command(&directory, "no-op", 9)
        .args(["--start-at-monotonic-ns", &start_at.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = report(&directory);
    assert_eq!(
        report["measurement_started_monotonic_ns"],
        start_at + 1_000_000_000
    );
    assert_eq!(
        report["measurement_ended_monotonic_ns"],
        start_at + 2_000_000_000
    );
}

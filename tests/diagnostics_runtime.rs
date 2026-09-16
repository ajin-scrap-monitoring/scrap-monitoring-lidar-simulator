use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use scrap_monitoring_lidar_simulator::{
    MAX_DIAGNOSTIC_SCANS_PER_SENSOR,
    configuration::{SimulatorInputs, load_simulator_inputs},
    diagnostics::{
        BoundedDiagnosticsWriter, DIAGNOSTICS_DRAIN_TIMEOUT, DIAGNOSTICS_QUEUE_CAPACITY,
        DiagnosticsRecordInput, DiagnosticsReferencePoint, DiagnosticsSubmit,
        DiagnosticsWriterConfig, MAX_DIAGNOSTICS_FILE_BYTES, MAX_DIAGNOSTICS_RECORD_BYTES,
        ReferenceHitKind, scan_captured_at_utc_us, simulator_input_fingerprint,
    },
    scenario::{ScenarioModelSnapshot, build_scenario_simulator},
};
use serde_json::Value;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "scrap-lidar-rust-{name}-{}-{sequence}",
            std::process::id()
        )))
    }

    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn inputs() -> SimulatorInputs {
    load_simulator_inputs(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"))
        .unwrap()
}

fn snapshot(elapsed_s: f64) -> ScenarioModelSnapshot {
    let mut simulator = build_scenario_simulator(&inputs()).unwrap();
    simulator.advance_to(elapsed_s).unwrap();
    simulator.observation_snapshot().unwrap()
}

fn config(output_directory: PathBuf, limit: u64) -> DiagnosticsWriterConfig {
    DiagnosticsWriterConfig::new(
        output_directory,
        "synthetic-scrap-pit-v1",
        "0".repeat(64),
        42,
        "run-a",
        1_800_000_000_000_000,
        vec!["lidar_1".into(), "lidar_2".into()],
        limit,
    )
    .unwrap()
}

fn record(sensor_id: &str, scan_id: u64, completed_at_s: f64) -> DiagnosticsRecordInput {
    DiagnosticsRecordInput::new(
        sensor_id,
        scan_id,
        0.0,
        completed_at_s,
        snapshot(completed_at_s),
        vec![
            DiagnosticsReferencePoint::new(0.0, 1.25, Some(ReferenceHitKind::Surface)).unwrap(),
            DiagnosticsReferencePoint::new(0.1, 0.0, None).unwrap(),
        ],
    )
    .unwrap()
}

#[test]
fn generation_fingerprint_matches_the_public_fixture() {
    assert_eq!(
        simulator_input_fingerprint(&inputs()).unwrap(),
        "611b752a910a2015d69eb827981f167f3194a7fa21c52d351b4222d39c9a2604"
    );
}

#[test]
fn writer_configuration_enforces_sixteen_records_per_sensor_without_changing_v2_loading() {
    assert_eq!(MAX_DIAGNOSTIC_SCANS_PER_SENSOR, 16);
    assert_eq!(DIAGNOSTICS_QUEUE_CAPACITY, 4);
    assert_eq!(MAX_DIAGNOSTICS_RECORD_BYTES, 4 * 1024 * 1024);
    assert_eq!(MAX_DIAGNOSTICS_FILE_BYTES, 64 * 1024 * 1024);
    assert_eq!(DIAGNOSTICS_DRAIN_TIMEOUT.as_secs(), 2);
    assert_eq!(
        inputs()
            .simulator
            .diagnostics
            .sample_scan_limit_per_sensor
            .get(),
        2
    );
    assert!(
        DiagnosticsWriterConfig::new(
            "diagnostics",
            "environment-a",
            "0".repeat(64),
            1,
            "run-a",
            0,
            vec!["lidar_1".into(), "lidar_2".into()],
            16,
        )
        .is_ok()
    );
    assert!(
        DiagnosticsWriterConfig::new(
            "diagnostics",
            "environment-a",
            "0".repeat(64),
            1,
            "run-a",
            0,
            vec!["lidar_1".into(), "lidar_2".into()],
            17,
        )
        .is_err()
    );
}

#[test]
fn captured_timestamp_uses_half_up_microsecond_rounding_and_checked_addition() {
    assert_eq!(scan_captured_at_utc_us(10, 0.000_000_4).unwrap(), 10);
    assert_eq!(scan_captured_at_utc_us(10, 0.000_000_5).unwrap(), 11);
    assert!(scan_captured_at_utc_us(i64::MAX, 0.000_001).is_err());
    assert!(scan_captured_at_utc_us(0, f64::INFINITY).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn writer_emits_v2_json_lines_with_owner_only_paths_and_exact_fields() {
    let temporary = TestDirectory::new("diagnostics-record");
    let output_directory = temporary.child("diagnostics");
    let mut writer = BoundedDiagnosticsWriter::start(config(output_directory.clone(), 2)).unwrap();
    assert_eq!(
        writer.try_record(record("lidar_1", 1, 0.1)).unwrap(),
        DiagnosticsSubmit::Enqueued
    );
    assert!(writer.close().await.drained);

    let output_path = writer.output_path().unwrap();
    assert_eq!(
        output_path.file_name().unwrap(),
        "reference-scans.v2.0001.jsonl"
    );
    assert_eq!(
        fs::metadata(&output_directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&output_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = fs::read(&output_path).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["diagnostics_version"], 2);
    assert_eq!(document["environment_id"], "synthetic-scrap-pit-v1");
    assert_eq!(document["run_id"], "run-a");
    assert_eq!(document["sensor_id"], "lidar_1");
    assert_eq!(document["scan_id"], 1);
    assert_eq!(document["captured_at"], 1_800_000_000_000_000_i64);
    assert_eq!(document["scenario"]["elapsed_s"], 0.1);
    assert_eq!(document["surface"]["snapshot_at_s"], 0.1);
    assert_eq!(document["surface"]["shape"], serde_json::json!([23, 17]));
    assert_eq!(
        document["reference_points"],
        serde_json::json!([[0.0, 1.25, "surface"], [0.1, 0.0, null]])
    );
    assert!(document.get("measured_points").is_none());
    assert_eq!(writer.recorded_counts().get("lidar_1"), Some(&1));
    assert_eq!(writer.stats().written_records, 1);
    assert_eq!(writer.stats().dropped_records, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn zero_limit_is_lazy_and_existing_files_are_never_overwritten() {
    let temporary = TestDirectory::new("diagnostics-files");
    let zero_path = temporary.child("zero");
    let mut zero = BoundedDiagnosticsWriter::start(config(zero_path.clone(), 0)).unwrap();
    assert_eq!(
        zero.try_record(record("lidar_1", 1, 0.1)).unwrap(),
        DiagnosticsSubmit::LimitReached
    );
    assert!(zero.close().await.drained);
    assert!(!zero_path.exists());

    let output_directory = temporary.child("numbered");
    let mut first = BoundedDiagnosticsWriter::start(config(output_directory.clone(), 1)).unwrap();
    assert_eq!(
        first.try_record(record("lidar_1", 1, 0.1)).unwrap(),
        DiagnosticsSubmit::Enqueued
    );
    assert!(first.close().await.drained);
    let first_path = first.output_path().unwrap();
    let first_contents = fs::read(&first_path).unwrap();

    let mut second = BoundedDiagnosticsWriter::start(config(output_directory, 1)).unwrap();
    assert_eq!(
        second.try_record(record("lidar_1", 1, 0.1)).unwrap(),
        DiagnosticsSubmit::Enqueued
    );
    assert!(second.close().await.drained);
    assert_eq!(
        second.output_path().unwrap().file_name().unwrap(),
        "reference-scans.v2.0002.jsonl"
    );
    assert_eq!(fs::read(first_path).unwrap(), first_contents);
    assert_eq!(
        fs::read(second.output_path().unwrap()).unwrap(),
        first_contents
    );
}

#[tokio::test(flavor = "current_thread")]
async fn per_sensor_selection_limit_and_closed_state_are_explicit() {
    let temporary = TestDirectory::new("diagnostics-limit");
    let mut writer = BoundedDiagnosticsWriter::start(config(temporary.child("output"), 1)).unwrap();
    assert_eq!(
        writer.try_record(record("lidar_1", 1, 0.1)).unwrap(),
        DiagnosticsSubmit::Enqueued
    );
    assert_eq!(
        writer.try_record(record("lidar_1", 2, 0.1)).unwrap(),
        DiagnosticsSubmit::LimitReached
    );
    assert!(!writer.wants_record("lidar_1").unwrap());
    assert!(writer.wants_record("lidar_2").unwrap());
    assert!(writer.try_record(record("unknown", 1, 0.1)).is_err());
    assert!(writer.close().await.drained);
    assert!(writer.try_record(record("lidar_2", 1, 0.1)).is_err());
    assert_eq!(writer.stats().selected_records, 1);
    assert_eq!(writer.stats().written_records, 1);
}

#[test]
fn diagnostics_input_rejects_a_stale_scenario_and_invalid_reference_semantics() {
    let stale = DiagnosticsRecordInput::new(
        "lidar_1",
        1,
        0.0,
        0.2,
        snapshot(0.1),
        vec![DiagnosticsReferencePoint::new(0.0, 1.0, Some(ReferenceHitKind::Floor)).unwrap()],
    );
    assert!(stale.is_err());
    assert!(DiagnosticsReferencePoint::new(360.0, 1.0, Some(ReferenceHitKind::Wall)).is_err());
    assert!(DiagnosticsReferencePoint::new(0.0, 0.0, Some(ReferenceHitKind::Floor)).is_err());
}

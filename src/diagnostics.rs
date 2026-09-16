//! Bounded asynchronous version 2 reference diagnostics.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, BufWriter, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::{Serialize, Serializer, ser::SerializeSeq};
use sha2::{Digest, Sha256};
use tokio::{sync::oneshot, time::timeout};

use crate::{
    MAX_DIAGNOSTIC_SCANS_PER_SENSOR,
    configuration::{MeasurementConfig, ScenarioConfig, SimulatorInputs},
    measurement::{HitKind, MeasurementResult},
    output_format::{DiagnosticsSurfaceDocument, ScenarioDocument, validate_model_snapshot},
    scenario::ScenarioModelSnapshot,
};

pub const DIAGNOSTICS_VERSION: u32 = 2;
pub const DIAGNOSTICS_QUEUE_CAPACITY: usize = 4;
pub const MAX_DIAGNOSTICS_RECORD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DIAGNOSTICS_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const DIAGNOSTICS_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_DIAGNOSTIC_REFERENCE_POINTS: usize = 32_768;

const MAX_SIGNED_64_BIT: i64 = i64::MAX;

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticsError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("diagnostics writer is closed")]
    Closed,
    #[error("diagnostics result sensor_id is not configured: {0}")]
    UnknownSensor(String),
    #[error("diagnostics scenario time must match scan completion")]
    ScenarioTimeMismatch,
    #[error("diagnostics JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("diagnostics worker could not start: {0}")]
    WorkerStart(#[source] io::Error),
    #[error("diagnostics point allocation failed")]
    Allocation,
    #[error("scan captured_at exceeds the signed 64-bit range")]
    TimestampOverflow,
}

pub type Result<T> = std::result::Result<T, DiagnosticsError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceHitKind {
    Floor,
    Wall,
    Surface,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticsReferencePoint {
    angle_deg: f64,
    distance_m: f64,
    hit_kind: Option<ReferenceHitKind>,
}

impl DiagnosticsReferencePoint {
    pub fn new(
        angle_deg: f64,
        distance_m: f64,
        hit_kind: Option<ReferenceHitKind>,
    ) -> Result<Self> {
        if !angle_deg.is_finite() || !(0.0..360.0).contains(&angle_deg) {
            return Err(DiagnosticsError::Invalid(
                "diagnostics reference angle must be finite and in [0, 360)",
            ));
        }
        if !distance_m.is_finite() || distance_m < 0.0 {
            return Err(DiagnosticsError::Invalid(
                "diagnostics reference distance must be finite and non-negative",
            ));
        }
        if (distance_m == 0.0) != hit_kind.is_none() {
            return Err(DiagnosticsError::Invalid(
                "only a diagnostics reference point without a hit may have zero distance",
            ));
        }
        Ok(Self {
            angle_deg,
            distance_m,
            hit_kind,
        })
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticsRecordInput {
    sensor_id: String,
    scan_id: u64,
    captured_elapsed_s: f64,
    completed_at_s: f64,
    snapshot: ScenarioModelSnapshot,
    reference_points: Arc<[DiagnosticsReferencePoint]>,
}

impl DiagnosticsRecordInput {
    pub fn new(
        sensor_id: impl Into<String>,
        scan_id: u64,
        captured_elapsed_s: f64,
        completed_at_s: f64,
        snapshot: ScenarioModelSnapshot,
        reference_points: Vec<DiagnosticsReferencePoint>,
    ) -> Result<Self> {
        let sensor_id = sensor_id.into();
        if sensor_id.is_empty() {
            return Err(DiagnosticsError::Invalid(
                "diagnostics sensor_id must be non-empty",
            ));
        }
        if scan_id == 0 || scan_id > MAX_SIGNED_64_BIT as u64 {
            return Err(DiagnosticsError::Invalid(
                "diagnostics scan_id must be a positive signed 64-bit integer",
            ));
        }
        if !captured_elapsed_s.is_finite() || captured_elapsed_s < 0.0 {
            return Err(DiagnosticsError::Invalid(
                "diagnostics captured elapsed time must be finite and non-negative",
            ));
        }
        if !completed_at_s.is_finite() || completed_at_s < 0.0 {
            return Err(DiagnosticsError::Invalid(
                "diagnostics completion time must be finite and non-negative",
            ));
        }
        if snapshot.state.elapsed_s != completed_at_s {
            return Err(DiagnosticsError::ScenarioTimeMismatch);
        }
        if reference_points.is_empty() || reference_points.len() > MAX_DIAGNOSTIC_REFERENCE_POINTS {
            return Err(DiagnosticsError::Invalid(
                "diagnostics reference point count must be from 1 through 32768",
            ));
        }
        Ok(Self {
            sensor_id,
            scan_id,
            captured_elapsed_s,
            completed_at_s,
            snapshot,
            reference_points: reference_points.into(),
        })
    }

    pub fn from_measurement(
        result: &MeasurementResult,
        snapshot: ScenarioModelSnapshot,
    ) -> Result<Self> {
        let reference = result.reference();
        let mut points = Vec::new();
        points
            .try_reserve_exact(reference.scan().points().len())
            .map_err(|_| DiagnosticsError::Allocation)?;
        for point in reference.scan().points() {
            let hit_kind = point.hit_kind.map(|kind| match kind {
                HitKind::Floor => ReferenceHitKind::Floor,
                HitKind::Wall => ReferenceHitKind::Wall,
                HitKind::Surface => ReferenceHitKind::Surface,
            });
            points.push(DiagnosticsReferencePoint::new(
                point.angle_deg,
                point.distance_m,
                hit_kind,
            )?);
        }
        Self::new(
            result.sensor_id(),
            result.scan_id(),
            reference.schedule().captured_elapsed_s(),
            reference.completed_at_s(),
            snapshot,
            points,
        )
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticsWriterConfig {
    output_directory: PathBuf,
    environment_id: String,
    input_fingerprint_sha256: String,
    seed: u64,
    run_id: String,
    run_started_at_utc_us: i64,
    sensor_ids: Vec<String>,
    sample_scan_limit_per_sensor: u64,
}

impl DiagnosticsWriterConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        output_directory: impl Into<PathBuf>,
        environment_id: impl Into<String>,
        input_fingerprint_sha256: impl Into<String>,
        seed: u64,
        run_id: impl Into<String>,
        run_started_at_utc_us: i64,
        sensor_ids: Vec<String>,
        sample_scan_limit_per_sensor: u64,
    ) -> Result<Self> {
        let environment_id = environment_id.into();
        let input_fingerprint_sha256 = input_fingerprint_sha256.into();
        let run_id = run_id.into();
        if environment_id.is_empty() {
            return Err(DiagnosticsError::Invalid(
                "diagnostics environment_id must be non-empty",
            ));
        }
        require_fingerprint(&input_fingerprint_sha256)?;
        if run_id.is_empty() {
            return Err(DiagnosticsError::Invalid(
                "diagnostics run_id must be non-empty",
            ));
        }
        if run_started_at_utc_us < 0 {
            return Err(DiagnosticsError::Invalid(
                "diagnostics run start UTC timestamp must be a non-negative signed 64-bit integer",
            ));
        }
        if sensor_ids.is_empty() || sensor_ids.iter().any(String::is_empty) {
            return Err(DiagnosticsError::Invalid(
                "diagnostics sensor identifiers must be non-empty",
            ));
        }
        if sensor_ids.iter().collect::<BTreeSet<_>>().len() != sensor_ids.len() {
            return Err(DiagnosticsError::Invalid(
                "diagnostics sensor identifiers must be unique",
            ));
        }
        if sample_scan_limit_per_sensor > MAX_DIAGNOSTIC_SCANS_PER_SENSOR {
            return Err(DiagnosticsError::Invalid(
                "diagnostics sample limit must not exceed 16 per sensor",
            ));
        }
        Ok(Self {
            output_directory: output_directory.into(),
            environment_id,
            input_fingerprint_sha256,
            seed,
            run_id,
            run_started_at_utc_us,
            sensor_ids,
            sample_scan_limit_per_sensor,
        })
    }

    pub fn from_inputs(
        inputs: &SimulatorInputs,
        run_id: impl Into<String>,
        run_started_at_utc_us: i64,
    ) -> Result<Self> {
        Self::new(
            inputs.simulator.diagnostics.output_path.clone(),
            inputs.environment.environment_id.clone(),
            simulator_input_fingerprint(inputs)?,
            inputs.simulator.seed,
            run_id,
            run_started_at_utc_us,
            inputs
                .environment
                .sensors
                .iter()
                .map(|sensor| sensor.sensor_id.clone())
                .collect(),
            inputs
                .simulator
                .diagnostics
                .sample_scan_limit_per_sensor
                .get(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticsWriterStats {
    pub selected_records: u64,
    pub enqueued_records: u64,
    pub written_records: u64,
    pub dropped_records: u64,
    pub failures: u64,
    pub writer_failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsSubmit {
    Enqueued,
    LimitReached,
    Dropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticsCloseOutcome {
    pub drained: bool,
}

#[derive(Default)]
struct WriterCounters {
    selected_records: AtomicU64,
    enqueued_records: AtomicU64,
    written_records: AtomicU64,
    dropped_records: AtomicU64,
    failures: AtomicU64,
    writer_failed: AtomicBool,
}

impl WriterCounters {
    fn snapshot(&self) -> DiagnosticsWriterStats {
        DiagnosticsWriterStats {
            selected_records: self.selected_records.load(Ordering::Relaxed),
            enqueued_records: self.enqueued_records.load(Ordering::Relaxed),
            written_records: self.written_records.load(Ordering::Relaxed),
            dropped_records: self.dropped_records.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            writer_failed: self.writer_failed.load(Ordering::Acquire),
        }
    }
}

struct WriterShared {
    counters: WriterCounters,
    output_path: Mutex<Option<PathBuf>>,
    written_counts: Mutex<BTreeMap<String, u64>>,
}

pub struct BoundedDiagnosticsWriter {
    sender: Mutex<Option<SyncSender<DiagnosticsRecordInput>>>,
    selected_counts: Mutex<BTreeMap<String, u64>>,
    sample_scan_limit_per_sensor: u64,
    closed: AtomicBool,
    shared: Arc<WriterShared>,
    done: Option<oneshot::Receiver<()>>,
    worker: Option<JoinHandle<()>>,
    drain_timeout: Duration,
    close_outcome: Option<DiagnosticsCloseOutcome>,
}

impl BoundedDiagnosticsWriter {
    pub fn start(config: DiagnosticsWriterConfig) -> Result<Self> {
        Self::start_inner(config, WriterLimits::PRODUCTION, None)
    }

    fn start_inner(
        config: DiagnosticsWriterConfig,
        limits: WriterLimits,
        gate: Option<Arc<WorkerGate>>,
    ) -> Result<Self> {
        let selected_counts = config
            .sensor_ids
            .iter()
            .cloned()
            .map(|sensor_id| (sensor_id, 0))
            .collect();
        let written_counts = config
            .sensor_ids
            .iter()
            .cloned()
            .map(|sensor_id| (sensor_id, 0))
            .collect();
        let sample_scan_limit_per_sensor = config.sample_scan_limit_per_sensor;
        let shared = Arc::new(WriterShared {
            counters: WriterCounters::default(),
            output_path: Mutex::new(None),
            written_counts: Mutex::new(written_counts),
        });
        let (sender, receiver) = sync_channel(DIAGNOSTICS_QUEUE_CAPACITY);
        let (done_sender, done) = oneshot::channel();
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("lidar-diagnostics".into())
            .spawn(move || {
                if let Some(gate) = gate {
                    gate.wait();
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(receiver, config, limits, &worker_shared);
                }));
                if result.is_err() {
                    mark_terminal_failure(&worker_shared.counters);
                }
                let _ = done_sender.send(());
            })
            .map_err(DiagnosticsError::WorkerStart)?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            selected_counts: Mutex::new(selected_counts),
            sample_scan_limit_per_sensor,
            closed: AtomicBool::new(false),
            shared,
            done: Some(done),
            worker: Some(worker),
            drain_timeout: limits.drain_timeout,
            close_outcome: None,
        })
    }

    pub fn try_record(&self, record: DiagnosticsRecordInput) -> Result<DiagnosticsSubmit> {
        if self.closed.load(Ordering::Acquire) {
            return Err(DiagnosticsError::Closed);
        }
        let mut counts = lock(&self.selected_counts);
        let Some(count) = counts.get_mut(&record.sensor_id) else {
            return Err(DiagnosticsError::UnknownSensor(record.sensor_id));
        };
        if *count >= self.sample_scan_limit_per_sensor {
            return Ok(DiagnosticsSubmit::LimitReached);
        }
        *count += 1;
        increment(&self.shared.counters.selected_records);
        drop(counts);

        if self.shared.counters.writer_failed.load(Ordering::Acquire) {
            increment(&self.shared.counters.dropped_records);
            return Ok(DiagnosticsSubmit::Dropped);
        }

        let sender = lock(&self.sender);
        let result = match sender.as_ref() {
            Some(sender) => sender.try_send(record),
            None => Err(TrySendError::Disconnected(record)),
        };
        match result {
            Ok(()) => {
                increment(&self.shared.counters.enqueued_records);
                Ok(DiagnosticsSubmit::Enqueued)
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                increment(&self.shared.counters.dropped_records);
                Ok(DiagnosticsSubmit::Dropped)
            }
        }
    }

    /// Report whether a producer should construct the next diagnostic snapshot for a sensor.
    pub fn wants_record(&self, sensor_id: &str) -> Result<bool> {
        if self.closed.load(Ordering::Acquire) {
            return Err(DiagnosticsError::Closed);
        }
        let counts = lock(&self.selected_counts);
        let Some(count) = counts.get(sensor_id) else {
            return Err(DiagnosticsError::UnknownSensor(sensor_id.into()));
        };
        Ok(!self.shared.counters.writer_failed.load(Ordering::Acquire)
            && *count < self.sample_scan_limit_per_sensor)
    }

    pub fn stats(&self) -> DiagnosticsWriterStats {
        self.shared.counters.snapshot()
    }

    pub fn output_path(&self) -> Option<PathBuf> {
        lock(&self.shared.output_path).clone()
    }

    pub fn recorded_counts(&self) -> BTreeMap<String, u64> {
        lock(&self.shared.written_counts).clone()
    }

    pub async fn close(&mut self) -> DiagnosticsCloseOutcome {
        if let Some(outcome) = self.close_outcome {
            return outcome;
        }
        self.closed.store(true, Ordering::Release);
        lock(&self.sender).take();
        let drained = match self.done.take() {
            Some(done) => matches!(timeout(self.drain_timeout, done).await, Ok(Ok(()))),
            None => true,
        };
        if let Some(worker) = self.worker.take()
            && drained
            && worker.is_finished()
        {
            let _ = worker.join();
        }
        let outcome = DiagnosticsCloseOutcome { drained };
        self.close_outcome = Some(outcome);
        outcome
    }
}

impl Drop for BoundedDiagnosticsWriter {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        lock(&self.sender).take();
        self.done.take();
        self.worker.take();
    }
}

#[derive(Clone, Copy)]
struct WriterLimits {
    record_bytes: usize,
    file_bytes: u64,
    drain_timeout: Duration,
}

impl WriterLimits {
    const PRODUCTION: Self = Self {
        record_bytes: MAX_DIAGNOSTICS_RECORD_BYTES,
        file_bytes: MAX_DIAGNOSTICS_FILE_BYTES,
        drain_timeout: DIAGNOSTICS_DRAIN_TIMEOUT,
    };
}

fn run_worker(
    receiver: Receiver<DiagnosticsRecordInput>,
    config: DiagnosticsWriterConfig,
    limits: WriterLimits,
    shared: &WriterShared,
) {
    let mut output = DiagnosticsFile::new(&config.output_directory, limits.file_bytes);
    while let Ok(record) = receiver.recv() {
        if shared.counters.writer_failed.load(Ordering::Acquire) {
            increment(&shared.counters.dropped_records);
            continue;
        }
        let frame = match encode_record(&config, &record, limits.record_bytes) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                increment(&shared.counters.dropped_records);
                continue;
            }
            Err(_) => {
                increment(&shared.counters.failures);
                increment(&shared.counters.dropped_records);
                continue;
            }
        };
        let write_result = output.write_record(&frame);
        if let Some(path) = output.path() {
            let mut output_path = lock(&shared.output_path);
            if output_path.is_none() {
                *output_path = Some(path.to_path_buf());
            }
        }
        match write_result {
            Ok(FileWrite::Written(path)) => {
                debug_assert_eq!(Some(path.as_path()), output.path());
                increment(&shared.counters.written_records);
                if let Some(count) = lock(&shared.written_counts).get_mut(&record.sensor_id) {
                    *count = count.saturating_add(1);
                }
            }
            Ok(FileWrite::CapacityReached) => {
                increment(&shared.counters.dropped_records);
            }
            Err(_) => {
                increment(&shared.counters.dropped_records);
                mark_terminal_failure(&shared.counters);
            }
        }
    }
}

enum FileWrite {
    Written(PathBuf),
    CapacityReached,
}

struct DiagnosticsFile {
    output_directory: PathBuf,
    stream: Option<BufWriter<File>>,
    path: Option<PathBuf>,
    written_bytes: u64,
    maximum_bytes: u64,
}

impl DiagnosticsFile {
    fn new(output_directory: &Path, maximum_bytes: u64) -> Self {
        Self {
            output_directory: output_directory.to_path_buf(),
            stream: None,
            path: None,
            written_bytes: 0,
            maximum_bytes,
        }
    }

    fn write_record(&mut self, frame: &[u8]) -> io::Result<FileWrite> {
        let frame_bytes = u64::try_from(frame.len())
            .map_err(|_| io::Error::other("diagnostics record length does not fit u64"))?;
        let Some(total_bytes) = self.written_bytes.checked_add(frame_bytes) else {
            return Ok(FileWrite::CapacityReached);
        };
        if total_bytes > self.maximum_bytes {
            return Ok(FileWrite::CapacityReached);
        }
        self.ensure_stream()?;
        let stream = self.stream.as_mut().expect("stream was initialized");
        stream.write_all(frame)?;
        stream.flush()?;
        self.written_bytes = total_bytes;
        Ok(FileWrite::Written(
            self.path.clone().expect("stream path was initialized"),
        ))
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn ensure_stream(&mut self) -> io::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let existed = self.output_directory.try_exists()?;
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(&self.output_directory)?;
        if !existed {
            fs::set_permissions(&self.output_directory, fs::Permissions::from_mode(0o700))?;
        }
        let mut sequence = 1_u64;
        loop {
            let candidate = self.output_directory.join(format!(
                "reference-scans.v{DIAGNOSTICS_VERSION}.{sequence:04}.jsonl"
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(file) => {
                    file.set_permissions(fs::Permissions::from_mode(0o600))?;
                    self.stream = Some(BufWriter::new(file));
                    self.path = Some(candidate);
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    sequence = sequence.checked_add(1).ok_or_else(|| {
                        io::Error::other("diagnostics file sequence is exhausted")
                    })?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[derive(Serialize)]
struct DiagnosticsDocument<'a> {
    diagnostics_version: u32,
    environment_id: &'a str,
    input_fingerprint_sha256: &'a str,
    seed: u64,
    run_id: &'a str,
    run_started_at_utc_us: i64,
    sensor_id: &'a str,
    scan_id: u64,
    captured_at: i64,
    captured_elapsed_s: f64,
    completed_at_s: f64,
    scenario: ScenarioDocument,
    surface: DiagnosticsSurfaceDocument<'a>,
    reference_points: ReferencePoints<'a>,
}

struct ReferencePoints<'a>(&'a [DiagnosticsReferencePoint]);

impl Serialize for ReferencePoints<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut points = serializer.serialize_seq(Some(self.0.len()))?;
        for point in self.0 {
            points.serialize_element(&(point.angle_deg, point.distance_m, point.hit_kind))?;
        }
        points.end()
    }
}

fn encode_record(
    config: &DiagnosticsWriterConfig,
    record: &DiagnosticsRecordInput,
    maximum_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    validate_model_snapshot(&record.snapshot).map_err(DiagnosticsError::Invalid)?;
    let captured_at =
        scan_captured_at_utc_us(config.run_started_at_utc_us, record.captured_elapsed_s)?;
    let document = DiagnosticsDocument {
        diagnostics_version: DIAGNOSTICS_VERSION,
        environment_id: &config.environment_id,
        input_fingerprint_sha256: &config.input_fingerprint_sha256,
        seed: config.seed,
        run_id: &config.run_id,
        run_started_at_utc_us: config.run_started_at_utc_us,
        sensor_id: &record.sensor_id,
        scan_id: record.scan_id,
        captured_at,
        captured_elapsed_s: record.captured_elapsed_s,
        completed_at_s: record.completed_at_s,
        scenario: ScenarioDocument::from(&record.snapshot.state),
        surface: DiagnosticsSurfaceDocument::new(&record.snapshot),
        reference_points: ReferencePoints(&record.reference_points),
    };
    let mut frame = serde_json::to_vec(&document)?;
    if frame.len().saturating_add(1) > maximum_bytes {
        return Ok(None);
    }
    frame.push(b'\n');
    Ok(Some(frame))
}

pub fn scan_captured_at_utc_us(run_started_at_utc_us: i64, captured_elapsed_s: f64) -> Result<i64> {
    if run_started_at_utc_us < 0 {
        return Err(DiagnosticsError::Invalid(
            "diagnostics run start UTC timestamp must be non-negative",
        ));
    }
    if !captured_elapsed_s.is_finite() || captured_elapsed_s < 0.0 {
        return Err(DiagnosticsError::Invalid(
            "scan captured elapsed time must be finite and non-negative",
        ));
    }
    let offset_us = (captured_elapsed_s * 1_000_000.0 + 0.5).floor();
    if !offset_us.is_finite() || offset_us >= 9_223_372_036_854_775_808.0 {
        return Err(DiagnosticsError::TimestampOverflow);
    }
    run_started_at_utc_us
        .checked_add(offset_us as i64)
        .ok_or(DiagnosticsError::TimestampOverflow)
}

#[derive(Serialize)]
struct FingerprintDocument<'a> {
    seed: u64,
    scenario: &'a ScenarioConfig,
    measurement: &'a MeasurementConfig,
    environment: &'a crate::configuration::EnvironmentConfig,
    quality_profile: FingerprintQualityProfile<'a>,
}

#[derive(Serialize)]
struct FingerprintQualityProfile<'a> {
    sensors: Vec<FingerprintSensorQuality<'a>>,
}

#[derive(Serialize)]
struct FingerprintSensorQuality<'a> {
    sensor_id: &'a str,
    valid_distance_frequencies: &'a [u64],
    invalid_distance_frequencies: &'a [u64],
}

pub fn simulator_input_fingerprint(inputs: &SimulatorInputs) -> Result<String> {
    let value = serde_json::to_value(FingerprintDocument {
        seed: inputs.simulator.seed,
        scenario: &inputs.simulator.scenario,
        measurement: &inputs.simulator.measurement,
        environment: &inputs.environment,
        quality_profile: FingerprintQualityProfile {
            sensors: inputs
                .quality_profile
                .sensors
                .iter()
                .map(|sensor| FingerprintSensorQuality {
                    sensor_id: &sensor.sensor_id,
                    valid_distance_frequencies: &sensor.valid_distance_frequencies,
                    invalid_distance_frequencies: &sensor.invalid_distance_frequencies,
                })
                .collect(),
        },
    })?;
    let payload = serde_json::to_vec(&value)?;
    let digest = Sha256::digest(payload);
    let mut fingerprint = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        fingerprint.push(HEX[usize::from(byte >> 4)] as char);
        fingerprint.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(fingerprint)
}

fn require_fingerprint(value: &str) -> Result<()> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(DiagnosticsError::Invalid(
            "diagnostics input fingerprint must be lowercase SHA-256 hex",
        ));
    }
    Ok(())
}

fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

fn mark_terminal_failure(counters: &WriterCounters) {
    if counters
        .writer_failed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        increment(&counters.failures);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct WorkerGate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl WorkerGate {
    #[cfg(test)]
    fn closed() -> Self {
        Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut open = lock(&self.open);
        while !*open {
            open = self
                .changed
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    fn release(&self) {
        *lock(&self.open) = true;
        self.changed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use crate::{
        configuration::load_simulator_inputs,
        scenario::{ScenarioModelSnapshot, build_scenario_simulator},
    };

    use super::*;

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "scrap-lidar-diagnostics-unit-{name}-{}-{sequence}",
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

    fn snapshot(elapsed_s: f64) -> ScenarioModelSnapshot {
        let inputs = load_simulator_inputs(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .unwrap();
        let mut simulator = build_scenario_simulator(&inputs).unwrap();
        simulator.advance_to(elapsed_s).unwrap();
        simulator.observation_snapshot().unwrap()
    }

    fn config(output: PathBuf, limit: u64) -> DiagnosticsWriterConfig {
        DiagnosticsWriterConfig::new(
            output,
            "environment-a",
            "0".repeat(64),
            42,
            "run-a",
            1_800_000_000_000_000,
            vec!["lidar_1".into(), "lidar_2".into()],
            limit,
        )
        .unwrap()
    }

    fn record(scan_id: u64) -> DiagnosticsRecordInput {
        DiagnosticsRecordInput::new(
            "lidar_1",
            scan_id,
            0.0,
            0.1,
            snapshot(0.1),
            vec![
                DiagnosticsReferencePoint::new(0.0, 1.0, Some(ReferenceHitKind::Surface)).unwrap(),
            ],
        )
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queue_capacity_drops_new_records_and_preserves_the_first_candidates() {
        let temporary = TestDirectory::new("queue");
        let gate = Arc::new(WorkerGate::closed());
        let mut writer = BoundedDiagnosticsWriter::start_inner(
            config(temporary.child("output"), 5),
            WriterLimits::PRODUCTION,
            Some(Arc::clone(&gate)),
        )
        .unwrap();

        for scan_id in 1..=4 {
            assert_eq!(
                writer.try_record(record(scan_id)).unwrap(),
                DiagnosticsSubmit::Enqueued
            );
        }
        assert_eq!(
            writer.try_record(record(5)).unwrap(),
            DiagnosticsSubmit::Dropped
        );
        assert_eq!(
            writer.try_record(record(6)).unwrap(),
            DiagnosticsSubmit::LimitReached
        );
        assert_eq!(writer.stats().selected_records, 5);
        assert_eq!(writer.stats().enqueued_records, 4);
        assert_eq!(writer.stats().dropped_records, 1);

        gate.release();
        assert!(writer.close().await.drained);
        assert_eq!(writer.stats().written_records, 4);
        let contents = fs::read_to_string(writer.output_path().unwrap()).unwrap();
        let scan_ids = contents
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["scan_id"].clone())
            .collect::<Vec<_>>();
        assert_eq!(scan_ids, [1, 2, 3, 4]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_returns_at_its_deadline_when_the_worker_cannot_drain() {
        let temporary = TestDirectory::new("drain");
        let gate = Arc::new(WorkerGate::closed());
        let limits = WriterLimits {
            drain_timeout: Duration::from_millis(20),
            ..WriterLimits::PRODUCTION
        };
        let mut writer = BoundedDiagnosticsWriter::start_inner(
            config(temporary.child("output"), 1),
            limits,
            Some(Arc::clone(&gate)),
        )
        .unwrap();
        assert_eq!(
            writer.try_record(record(1)).unwrap(),
            DiagnosticsSubmit::Enqueued
        );

        let started = Instant::now();
        assert!(!writer.close().await.drained);
        assert!(started.elapsed() < Duration::from_millis(200));
        gate.release();
        for _ in 0..100 {
            if writer.stats().written_records == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(writer.stats().written_records, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_and_file_byte_limits_drop_whole_records_without_allocating_a_file() {
        for (name, limits) in [
            (
                "record",
                WriterLimits {
                    record_bytes: 10,
                    ..WriterLimits::PRODUCTION
                },
            ),
            (
                "file",
                WriterLimits {
                    file_bytes: 10,
                    ..WriterLimits::PRODUCTION
                },
            ),
        ] {
            let temporary = TestDirectory::new(name);
            let output = temporary.child("output");
            let mut writer =
                BoundedDiagnosticsWriter::start_inner(config(output.clone(), 1), limits, None)
                    .unwrap();
            assert_eq!(
                writer.try_record(record(1)).unwrap(),
                DiagnosticsSubmit::Enqueued
            );
            assert!(writer.close().await.drained);
            assert_eq!(writer.stats().written_records, 0);
            assert_eq!(writer.stats().dropped_records, 1);
            assert!(!output.exists());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn filesystem_failure_is_terminal_and_drops_later_queued_records() {
        let temporary = TestDirectory::new("failure");
        fs::create_dir_all(&temporary.0).unwrap();
        let invalid_directory = temporary.child("not-a-directory");
        fs::write(&invalid_directory, b"file").unwrap();
        let gate = Arc::new(WorkerGate::closed());
        let mut writer = BoundedDiagnosticsWriter::start_inner(
            config(invalid_directory, 3),
            WriterLimits::PRODUCTION,
            Some(Arc::clone(&gate)),
        )
        .unwrap();
        assert_eq!(
            writer.try_record(record(1)).unwrap(),
            DiagnosticsSubmit::Enqueued
        );
        assert_eq!(
            writer.try_record(record(2)).unwrap(),
            DiagnosticsSubmit::Enqueued
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !writer.stats().writer_failed {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            writer.try_record(record(3)).unwrap(),
            DiagnosticsSubmit::Dropped
        );
        assert!(writer.close().await.drained);

        let stats = writer.stats();
        assert!(stats.writer_failed);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.written_records, 0);
        assert_eq!(stats.enqueued_records, 2);
        assert_eq!(stats.dropped_records, 3);
    }
}

//! Best-effort version 1 load-model observation streaming over TCP.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::Serialize;
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::Notify,
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{
    configuration::{ObservationTransportConfig, SimulatorInputs},
    output_format::{ObservationSurfaceDocument, ScenarioDocument, validate_model_snapshot},
    randomness::{ModelRng, RandomError, RandomSource, StreamScope},
    scenario::ScenarioModelSnapshot,
};

pub const OBSERVATION_VERSION: u32 = 1;
pub const MAX_OBSERVATION_LINE_BYTES: usize = 1_048_576;
pub const MAX_OBSERVATION_SEQUENCE: u64 = 9_007_199_254_740_991;
pub const DEFAULT_OBSERVATION_HOST: &str = "127.0.0.1";
pub const DEFAULT_OBSERVATION_PORT: u16 = 17_000;
pub const DEFAULT_OBSERVATION_INTERVAL_S: f64 = 1.0;
pub const MAX_OBSERVATION_INTERVAL_S: f64 = 86_400.0;

const TIME_TOLERANCE_S: f64 = 1e-12;

#[derive(Debug, thiserror::Error)]
pub enum ObservationError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("observation JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("observation line exceeds {maximum} bytes: {actual}")]
    LineTooLarge { actual: usize, maximum: usize },
    #[error("observation sequence range is exhausted")]
    SequenceExhausted,
    #[error("observation publisher is closed")]
    Closed,
    #[error("observation publisher requires a Tokio runtime")]
    RuntimeUnavailable,
    #[error(transparent)]
    Random(#[from] RandomError),
}

pub type Result<T> = std::result::Result<T, ObservationError>;

#[derive(Clone, Debug, PartialEq)]
pub struct ObservationSensor {
    sensor_id: String,
    p0_m: [f64; 3],
    u0: [f64; 3],
    u90: [f64; 3],
}

impl ObservationSensor {
    pub fn new(
        sensor_id: impl Into<String>,
        p0_m: [f64; 3],
        u0: [f64; 3],
        u90: [f64; 3],
    ) -> Result<Self> {
        let sensor_id = sensor_id.into();
        if sensor_id.is_empty() {
            return Err(ObservationError::Invalid(
                "observation sensor_id must be non-empty",
            ));
        }
        if [p0_m, u0, u90]
            .iter()
            .flatten()
            .any(|component| !component.is_finite())
        {
            return Err(ObservationError::Invalid(
                "observation sensor vectors must contain finite numbers",
            ));
        }
        Ok(Self {
            sensor_id,
            p0_m,
            u0,
            u90,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObservationScene {
    boundary_xy_m: Vec<[f64; 2]>,
    floor_z_m: f64,
    top_z_m: f64,
    inlet_positions_xy_m: Vec<[f64; 2]>,
    sensors: Vec<ObservationSensor>,
}

impl ObservationScene {
    pub fn new(
        boundary_xy_m: Vec<[f64; 2]>,
        floor_z_m: f64,
        top_z_m: f64,
        inlet_positions_xy_m: Vec<[f64; 2]>,
        sensors: Vec<ObservationSensor>,
    ) -> Result<Self> {
        if boundary_xy_m.len() < 3
            || boundary_xy_m
                .iter()
                .flatten()
                .any(|component| !component.is_finite())
        {
            return Err(ObservationError::Invalid(
                "observation boundary must contain at least 3 finite coordinates",
            ));
        }
        if !floor_z_m.is_finite() || !top_z_m.is_finite() || top_z_m <= floor_z_m {
            return Err(ObservationError::Invalid(
                "observation top must be finite and above its finite floor",
            ));
        }
        if inlet_positions_xy_m.is_empty()
            || inlet_positions_xy_m
                .iter()
                .flatten()
                .any(|component| !component.is_finite())
        {
            return Err(ObservationError::Invalid(
                "observation scene must contain at least 1 finite inlet",
            ));
        }
        if sensors.is_empty() {
            return Err(ObservationError::Invalid(
                "observation scene must contain at least 1 sensor",
            ));
        }
        let sensor_ids = sensors
            .iter()
            .map(|sensor| sensor.sensor_id.as_str())
            .collect::<BTreeSet<_>>();
        if sensor_ids.len() != sensors.len() {
            return Err(ObservationError::Invalid(
                "observation sensor identifiers must be unique",
            ));
        }
        Ok(Self {
            boundary_xy_m,
            floor_z_m,
            top_z_m,
            inlet_positions_xy_m,
            sensors,
        })
    }

    pub fn from_inputs(inputs: &SimulatorInputs) -> Result<Self> {
        let sensors = inputs
            .environment
            .sensors
            .iter()
            .map(|sensor| {
                ObservationSensor::new(sensor.sensor_id.clone(), sensor.p0_m, sensor.u0, sensor.u90)
            })
            .collect::<Result<Vec<_>>>()?;
        Self::new(
            inputs.environment.boundary_xy_m.clone(),
            inputs.environment.floor_z_m,
            inputs.environment.top_z_m,
            inputs.simulator.scenario.inlet_positions_xy_m.clone(),
            sensors,
        )
    }
}

#[derive(Clone, Debug)]
pub struct ObservationStreamHeader {
    environment_id: String,
    run_id: String,
    input_fingerprint_sha256: String,
    seed: u64,
    scene: ObservationScene,
}

impl ObservationStreamHeader {
    pub fn new(
        environment_id: impl Into<String>,
        run_id: impl Into<String>,
        input_fingerprint_sha256: impl Into<String>,
        seed: u64,
        scene: ObservationScene,
    ) -> Result<Self> {
        let environment_id = environment_id.into();
        let run_id = run_id.into();
        let input_fingerprint_sha256 = input_fingerprint_sha256.into();
        if environment_id.is_empty() {
            return Err(ObservationError::Invalid(
                "observation environment_id must be non-empty",
            ));
        }
        if run_id.is_empty() {
            return Err(ObservationError::Invalid(
                "observation run_id must be non-empty",
            ));
        }
        require_fingerprint(&input_fingerprint_sha256)?;
        Ok(Self {
            environment_id,
            run_id,
            input_fingerprint_sha256,
            seed,
            scene,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ObservationRecord {
    sequence: u64,
    run_id: String,
    snapshot: ScenarioModelSnapshot,
}

impl ObservationRecord {
    pub fn new(
        sequence: u64,
        run_id: impl Into<String>,
        snapshot: ScenarioModelSnapshot,
    ) -> Result<Self> {
        if !(1..=MAX_OBSERVATION_SEQUENCE).contains(&sequence) {
            return Err(ObservationError::Invalid(
                "observation sequence must be a positive JSON-safe integer",
            ));
        }
        let run_id = run_id.into();
        if run_id.is_empty() {
            return Err(ObservationError::Invalid(
                "observation run_id must be non-empty",
            ));
        }
        Ok(Self {
            sequence,
            run_id,
            snapshot,
        })
    }
}

#[derive(Serialize)]
struct HeaderDocument<'a> {
    observation_version: u32,
    #[serde(rename = "type")]
    record_type: &'static str,
    environment_id: &'a str,
    run_id: &'a str,
    input_fingerprint_sha256: &'a str,
    seed: u64,
    scene: SceneDocument<'a>,
}

#[derive(Serialize)]
struct SceneDocument<'a> {
    coordinate_system: &'static str,
    length_unit: &'static str,
    angle_unit: &'static str,
    boundary_xy_m: &'a [[f64; 2]],
    floor_z_m: f64,
    top_z_m: f64,
    inlet_positions_xy_m: &'a [[f64; 2]],
    sensors: Vec<SensorDocument<'a>>,
}

#[derive(Serialize)]
struct SensorDocument<'a> {
    sensor_id: &'a str,
    p0_m: [f64; 3],
    u0: [f64; 3],
    u90: [f64; 3],
}

#[derive(Serialize)]
struct RecordDocument<'a> {
    observation_version: u32,
    #[serde(rename = "type")]
    record_type: &'static str,
    sequence: u64,
    run_id: &'a str,
    scenario: ScenarioDocument,
    surface: ObservationSurfaceDocument<'a>,
}

pub fn encode_observation_header_frame(header: &ObservationStreamHeader) -> Result<Vec<u8>> {
    let scene = &header.scene;
    let document = HeaderDocument {
        observation_version: OBSERVATION_VERSION,
        record_type: "load_model_stream_header",
        environment_id: &header.environment_id,
        run_id: &header.run_id,
        input_fingerprint_sha256: &header.input_fingerprint_sha256,
        seed: header.seed,
        scene: SceneDocument {
            coordinate_system: "right-handed-z-up",
            length_unit: "m",
            angle_unit: "deg",
            boundary_xy_m: &scene.boundary_xy_m,
            floor_z_m: scene.floor_z_m,
            top_z_m: scene.top_z_m,
            inlet_positions_xy_m: &scene.inlet_positions_xy_m,
            sensors: scene
                .sensors
                .iter()
                .map(|sensor| SensorDocument {
                    sensor_id: &sensor.sensor_id,
                    p0_m: sensor.p0_m,
                    u0: sensor.u0,
                    u90: sensor.u90,
                })
                .collect(),
        },
    };
    encode_frame(&document)
}

pub fn encode_observation_record_frame(record: &ObservationRecord) -> Result<Vec<u8>> {
    validate_model_snapshot(&record.snapshot).map_err(ObservationError::Invalid)?;
    let document = RecordDocument {
        observation_version: OBSERVATION_VERSION,
        record_type: "load_model_observation",
        sequence: record.sequence,
        run_id: &record.run_id,
        scenario: ScenarioDocument::from(&record.snapshot.state),
        surface: ObservationSurfaceDocument::from(&record.snapshot.surface),
    };
    encode_frame(&document)
}

fn encode_frame(document: &impl Serialize) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(document)?;
    let actual = encoded.len().saturating_add(1);
    if actual > MAX_OBSERVATION_LINE_BYTES {
        return Err(ObservationError::LineTooLarge {
            actual,
            maximum: MAX_OBSERVATION_LINE_BYTES,
        });
    }
    encoded.push(b'\n');
    Ok(encoded)
}

#[derive(Clone, Debug)]
pub struct ObservationPublisherConfig {
    host: String,
    port: u16,
    interval_s: f64,
    connect_timeout: Duration,
    send_timeout: Duration,
    reconnect_initial_delay_s: f64,
    reconnect_max_delay_s: f64,
}

impl ObservationPublisherConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        interval_s: f64,
        connect_timeout_s: f64,
        send_timeout_s: f64,
        reconnect_initial_delay_s: f64,
        reconnect_max_delay_s: f64,
    ) -> Result<Self> {
        let host = host.into();
        if host.is_empty() {
            return Err(ObservationError::Invalid(
                "observation host must be non-empty",
            ));
        }
        if port == 0 {
            return Err(ObservationError::Invalid(
                "observation port must be from 1 through 65535",
            ));
        }
        if !interval_s.is_finite() || interval_s <= 0.0 || interval_s > MAX_OBSERVATION_INTERVAL_S {
            return Err(ObservationError::Invalid(
                "observation interval must be finite and between 0 and 86400 seconds",
            ));
        }
        let connect_timeout = positive_duration(connect_timeout_s, "connect timeout")?;
        let send_timeout = positive_duration(send_timeout_s, "send timeout")?;
        if !reconnect_initial_delay_s.is_finite() || reconnect_initial_delay_s <= 0.0 {
            return Err(ObservationError::Invalid(
                "observation reconnect initial delay must be finite and positive",
            ));
        }
        if !reconnect_max_delay_s.is_finite()
            || reconnect_max_delay_s <= 0.0
            || reconnect_initial_delay_s > reconnect_max_delay_s
        {
            return Err(ObservationError::Invalid(
                "observation reconnect maximum delay must be finite, positive, and not below the initial delay",
            ));
        }
        positive_duration(reconnect_initial_delay_s, "reconnect initial delay")?;
        positive_duration(reconnect_max_delay_s, "reconnect maximum delay")?;
        Ok(Self {
            host,
            port,
            interval_s,
            connect_timeout,
            send_timeout,
            reconnect_initial_delay_s,
            reconnect_max_delay_s,
        })
    }

    pub fn from_transport(
        host: impl Into<String>,
        port: u16,
        interval_s: f64,
        transport: &ObservationTransportConfig,
    ) -> Result<Self> {
        Self::new(
            host,
            port,
            interval_s,
            transport.connect_timeout_s,
            transport.send_timeout_s,
            transport.reconnect_initial_delay_s,
            transport.reconnect_max_delay_s,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservationPublisherStats {
    pub accepted_records: u64,
    pub sent_records: u64,
    pub dropped_records: u64,
    pub connection_failures: u64,
}

#[derive(Default)]
struct PublisherCounters {
    accepted_records: AtomicU64,
    sent_records: AtomicU64,
    dropped_records: AtomicU64,
    connection_failures: AtomicU64,
}

impl PublisherCounters {
    fn snapshot(&self) -> ObservationPublisherStats {
        ObservationPublisherStats {
            accepted_records: self.accepted_records.load(Ordering::Relaxed),
            sent_records: self.sent_records.load(Ordering::Relaxed),
            dropped_records: self.dropped_records.load(Ordering::Relaxed),
            connection_failures: self.connection_failures.load(Ordering::Relaxed),
        }
    }
}

struct PublisherState {
    closed: bool,
    latest: Option<ObservationRecord>,
    next_sequence: u64,
    last_elapsed_s: Option<f64>,
    next_sample_s: f64,
}

struct SharedPublisher {
    state: Mutex<PublisherState>,
    counters: PublisherCounters,
    wakeup: Notify,
}

pub struct TcpObservationPublisher {
    config: ObservationPublisherConfig,
    run_id: String,
    seed: u64,
    header_frame: Arc<[u8]>,
    shared: Arc<SharedPublisher>,
    task: Option<JoinHandle<()>>,
}

impl TcpObservationPublisher {
    pub fn new(
        config: ObservationPublisherConfig,
        header: ObservationStreamHeader,
    ) -> Result<Self> {
        let header_frame = encode_observation_header_frame(&header)?;
        Ok(Self {
            config,
            run_id: header.run_id,
            seed: header.seed,
            header_frame: header_frame.into(),
            shared: Arc::new(SharedPublisher {
                state: Mutex::new(PublisherState {
                    closed: false,
                    latest: None,
                    next_sequence: 1,
                    last_elapsed_s: None,
                    next_sample_s: 0.0,
                }),
                counters: PublisherCounters::default(),
                wakeup: Notify::new(),
            }),
            task: None,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.config.host, self.config.port)
    }

    pub fn stats(&self) -> ObservationPublisherStats {
        self.shared.counters.snapshot()
    }

    pub fn is_due(&self, elapsed_s: f64) -> bool {
        let mut state = lock(&self.shared.state);
        if state.closed || !elapsed_s.is_finite() {
            return false;
        }
        if state
            .last_elapsed_s
            .is_some_and(|last| elapsed_s + TIME_TOLERANCE_S < last)
        {
            return false;
        }
        state.last_elapsed_s = Some(elapsed_s);
        if elapsed_s + TIME_TOLERANCE_S < state.next_sample_s {
            return false;
        }
        state.next_sample_s = (elapsed_s / self.config.interval_s + TIME_TOLERANCE_S).floor()
            * self.config.interval_s
            + self.config.interval_s;
        true
    }

    pub fn publish(&self, snapshot: ScenarioModelSnapshot) -> Result<bool> {
        let mut state = lock(&self.shared.state);
        if state.closed {
            return Ok(false);
        }
        if state.next_sequence > MAX_OBSERVATION_SEQUENCE {
            return Err(ObservationError::SequenceExhausted);
        }
        let sequence = state.next_sequence;
        state.next_sequence += 1;
        let record = ObservationRecord::new(sequence, self.run_id.clone(), snapshot)?;
        if state.latest.replace(record).is_some() {
            increment(&self.shared.counters.dropped_records);
        }
        increment(&self.shared.counters.accepted_records);
        drop(state);
        self.shared.wakeup.notify_one();
        Ok(true)
    }

    pub fn start(&mut self) -> Result<()> {
        if lock(&self.shared.state).closed {
            return Err(ObservationError::Closed);
        }
        if self.task.is_some() {
            return Ok(());
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ObservationError::RuntimeUnavailable)?;
        let config = self.config.clone();
        let header_frame = Arc::clone(&self.header_frame);
        let shared = Arc::clone(&self.shared);
        let mut backoff = ReconnectBackoff::new(
            config.reconnect_initial_delay_s,
            config.reconnect_max_delay_s,
            self.seed,
        )?;
        self.task = Some(runtime.spawn(async move {
            run_publisher(config, header_frame, shared, &mut backoff).await;
        }));
        Ok(())
    }

    pub async fn close(&mut self) {
        close_shared(&self.shared);
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        reconcile_accepted_records(&self.shared.counters);
    }
}

impl Drop for TcpObservationPublisher {
    fn drop(&mut self) {
        close_shared(&self.shared);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run_publisher(
    config: ObservationPublisherConfig,
    header_frame: Arc<[u8]>,
    shared: Arc<SharedPublisher>,
    backoff: &mut ReconnectBackoff,
) {
    loop {
        if !wait_for_pending(&shared).await {
            return;
        }
        let result = use_next_connection(&config, &header_frame, &shared, backoff).await;
        if result.is_err() {
            increment(&shared.counters.connection_failures);
        }
        if lock(&shared.state).closed {
            return;
        }
        sleep(backoff.next_delay_after_failure()).await;
    }
}

async fn use_next_connection(
    config: &ObservationPublisherConfig,
    header_frame: &[u8],
    shared: &SharedPublisher,
    backoff: &mut ReconnectBackoff,
) -> std::io::Result<()> {
    let mut stream = timeout(
        config.connect_timeout,
        TcpStream::connect((config.host.as_str(), config.port)),
    )
    .await
    .map_err(|_| timed_out("observation connect timed out"))??;
    write_frame(&mut stream, header_frame, config.send_timeout).await?;

    loop {
        if !wait_for_pending(shared).await {
            return Ok(());
        }
        let Some(record) = take_latest(shared) else {
            continue;
        };
        let frame = match encode_observation_record_frame(&record) {
            Ok(frame) => frame,
            Err(_) => {
                increment(&shared.counters.dropped_records);
                continue;
            }
        };
        if let Err(error) = write_frame(&mut stream, &frame, config.send_timeout).await {
            increment(&shared.counters.dropped_records);
            return Err(error);
        }
        increment(&shared.counters.sent_records);
        backoff.reset_after_success();
    }
}

async fn write_frame(
    stream: &mut TcpStream,
    frame: &[u8],
    deadline: Duration,
) -> std::io::Result<()> {
    timeout(deadline, stream.write_all(frame))
        .await
        .map_err(|_| timed_out("observation send timed out"))?
}

async fn wait_for_pending(shared: &SharedPublisher) -> bool {
    loop {
        let notified = shared.wakeup.notified();
        {
            let state = lock(&shared.state);
            if state.closed {
                return false;
            }
            if state.latest.is_some() {
                return true;
            }
        }
        notified.await;
    }
}

fn take_latest(shared: &SharedPublisher) -> Option<ObservationRecord> {
    lock(&shared.state).latest.take()
}

fn close_shared(shared: &SharedPublisher) {
    let mut state = lock(&shared.state);
    if state.closed {
        return;
    }
    state.closed = true;
    if state.latest.take().is_some() {
        increment(&shared.counters.dropped_records);
    }
    drop(state);
    shared.wakeup.notify_one();
}

fn positive_duration(value: f64, _name: &'static str) -> Result<Duration> {
    if !value.is_finite() || value <= 0.0 {
        return Err(ObservationError::Invalid(
            "observation timeout must be finite and positive",
        ));
    }
    Duration::try_from_secs_f64(value).map_err(|_| {
        ObservationError::Invalid("observation timeout must fit the platform duration range")
    })
}

fn require_fingerprint(value: &str) -> Result<()> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(ObservationError::Invalid(
            "observation input fingerprint must be lowercase SHA-256 hex",
        ));
    }
    Ok(())
}

fn increment(counter: &AtomicU64) {
    add(counter, 1);
}

fn add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

fn reconcile_accepted_records(counters: &PublisherCounters) {
    let accepted = counters.accepted_records.load(Ordering::Relaxed);
    let accounted = counters
        .sent_records
        .load(Ordering::Relaxed)
        .saturating_add(counters.dropped_records.load(Ordering::Relaxed));
    add(
        &counters.dropped_records,
        accepted.saturating_sub(accounted),
    );
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn timed_out(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, message)
}

struct ReconnectBackoff {
    initial_delay_s: f64,
    maximum_delay_s: f64,
    current_delay_cap_s: f64,
    consecutive_failures: u64,
    rng: ModelRng,
}

impl ReconnectBackoff {
    fn new(initial_delay_s: f64, maximum_delay_s: f64, seed: u64) -> Result<Self> {
        Ok(Self {
            initial_delay_s,
            maximum_delay_s,
            current_delay_cap_s: initial_delay_s,
            consecutive_failures: 0,
            rng: ModelRng::new(seed, StreamScope::Global, "observation-reconnect-jitter")?,
        })
    }

    fn next_delay_after_failure(&mut self) -> Duration {
        let delay_s = self.rng.unit_f64() * self.current_delay_cap_s;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.current_delay_cap_s = (self.current_delay_cap_s * 2.0).min(self.maximum_delay_s);
        Duration::from_secs_f64(delay_s)
    }

    fn reset_after_success(&mut self) {
        self.current_delay_cap_s = self.initial_delay_s;
        self.consecutive_failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{configuration::load_simulator_inputs, scenario::build_scenario_simulator};

    use super::*;

    fn publisher() -> TcpObservationPublisher {
        let inputs = load_simulator_inputs(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .unwrap();
        let scene = ObservationScene::from_inputs(&inputs).unwrap();
        let header = ObservationStreamHeader::new(
            inputs.environment.environment_id.clone(),
            "run-a",
            "0".repeat(64),
            inputs.simulator.seed,
            scene,
        )
        .unwrap();
        let config =
            ObservationPublisherConfig::new("127.0.0.1", 17_000, 1.0, 1.0, 1.0, 0.5, 5.0).unwrap();
        TcpObservationPublisher::new(config, header).unwrap()
    }

    #[test]
    fn sequence_stops_at_the_json_safe_integer_limit() {
        let publisher = publisher();
        lock(&publisher.shared.state).next_sequence = MAX_OBSERVATION_SEQUENCE;
        let inputs = load_simulator_inputs(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .unwrap();
        let simulator = build_scenario_simulator(&inputs).unwrap();
        assert!(
            publisher
                .publish(simulator.observation_snapshot().unwrap())
                .unwrap()
        );
        assert!(matches!(
            publisher.publish(simulator.observation_snapshot().unwrap()),
            Err(ObservationError::SequenceExhausted)
        ));
    }

    #[test]
    fn full_jitter_is_deterministic_and_success_resets_only_the_cap() {
        let mut first = ReconnectBackoff::new(0.5, 2.0, 42).unwrap();
        let mut second = ReconnectBackoff::new(0.5, 2.0, 42).unwrap();
        for cap in [0.5, 1.0, 2.0, 2.0] {
            let first_delay = first.next_delay_after_failure().as_secs_f64();
            let second_delay = second.next_delay_after_failure().as_secs_f64();
            assert_eq!(first_delay, second_delay);
            assert!((0.0..cap).contains(&first_delay));
        }
        first.reset_after_success();
        assert_eq!(first.current_delay_cap_s, 0.5);
        assert_eq!(first.consecutive_failures, 0);
        assert!(first.next_delay_after_failure().as_secs_f64() < 0.5);
    }

    #[test]
    fn close_reconciliation_accounts_for_an_in_flight_record_once() {
        let counters = PublisherCounters::default();
        counters.accepted_records.store(3, Ordering::Relaxed);
        counters.sent_records.store(1, Ordering::Relaxed);

        reconcile_accepted_records(&counters);
        reconcile_accepted_records(&counters);

        assert_eq!(counters.snapshot().sent_records, 1);
        assert_eq!(counters.snapshot().dropped_records, 2);
    }
}

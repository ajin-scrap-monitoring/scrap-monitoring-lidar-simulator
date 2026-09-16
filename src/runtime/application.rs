//! Process lifecycle for paced generation and independent external outputs.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::{
    cli::RuntimeSettings,
    configuration::SimulatorInputs,
    diagnostics::{
        BoundedDiagnosticsWriter, DiagnosticsError, DiagnosticsRecordInput,
        DiagnosticsWriterConfig, simulator_input_fingerprint,
    },
    measurement::{MeasurementError, SystemScanFrameFactory},
    observation::{
        ObservationError, ObservationPublisherConfig, ObservationPublisherStats, ObservationScene,
        ObservationStreamHeader, TcpObservationPublisher,
    },
    scan_runtime::{GrpcScanRuntime, ScanRuntimeConfig, ScanRuntimeError, ScanRuntimeStats},
};

use super::{GenerationBatch, GenerationRuntime, GenerationRuntimeError};

#[cfg(feature = "edge-validation")]
use super::edge_validation::{
    EdgeValidationError, EdgeValidationObservationMode, EdgeValidationRecorder,
    EdgeValidationReport, EdgeValidationSettings, PublishedFrameTiming,
    write_edge_validation_report_atomic,
};

const GENERATION_OUTPUT_CAPACITY: usize = 1;
#[cfg(feature = "edge-validation")]
const EDGE_VALIDATION_POST_MEASUREMENT_HOLD: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error(transparent)]
    Diagnostics(#[from] DiagnosticsError),
    #[error(transparent)]
    Generation(#[from] GenerationRuntimeError),
    #[error(transparent)]
    Measurement(#[from] MeasurementError),
    #[error(transparent)]
    Observation(#[from] ObservationError),
    #[error(transparent)]
    Scan(#[from] ScanRuntimeError),
    #[error("generation coordinator could not start: {0}")]
    GenerationStart(#[source] std::io::Error),
    #[error("generation coordinator failed: {0}")]
    GenerationWorker(String),
    #[error("generation coordinator panicked")]
    GenerationPanic,
    #[error("generation coordinator join task failed: {0}")]
    GenerationJoin(#[source] tokio::task::JoinError),
    #[error("runtime signal registration failed: {0}")]
    Signal(#[source] std::io::Error),
    #[error("scan runtime failed: {0}")]
    ScanFatal(String),
    #[error("system wall clock precedes the Unix epoch")]
    ClockBeforeEpoch,
    #[error("system wall clock microseconds exceed the signed 64-bit range")]
    ClockOverflow,
    #[error("host clock mapping failed: {0}")]
    ClockMapping(&'static str),
    #[error("generation deadline is outside the host monotonic clock range")]
    DeadlineOverflow,
    #[error("generation coordinator stopped unexpectedly")]
    GenerationStopped,
    #[cfg(feature = "edge-validation")]
    #[error(transparent)]
    EdgeValidation(#[from] EdgeValidationError),
}

pub type Result<T> = std::result::Result<T, ApplicationError>;

#[derive(Clone, Debug)]
pub struct ApplicationSummary {
    pub run_id: String,
    pub run_started_at_utc_us: i64,
    pub generated_scans: u64,
    pub scan_stats: ScanRuntimeStats,
    pub observation_endpoint: String,
    pub observation_stats: ObservationPublisherStats,
}

struct GeneratedOutput {
    batch: GenerationBatch,
    #[cfg(feature = "edge-validation")]
    deadline_monotonic_ns: u64,
    snapshot: Option<crate::scenario::ScenarioModelSnapshot>,
    observation_due: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClockAnchor {
    monotonic_ns: u64,
    unix_utc_us: i64,
}

impl ClockAnchor {
    fn unix_utc_us_at(self, monotonic_ns: u64) -> Result<i64> {
        let offset_ns =
            monotonic_ns
                .checked_sub(self.monotonic_ns)
                .ok_or(ApplicationError::ClockMapping(
                    "generation epoch precedes the clock anchor",
                ))?;
        let offset_us = offset_ns
            .checked_add(500)
            .ok_or(ApplicationError::ClockMapping(
                "generation epoch offset exceeds u64",
            ))?
            / 1_000;
        let offset_us = i64::try_from(offset_us).map_err(|_| {
            ApplicationError::ClockMapping("generation epoch offset exceeds i64 microseconds")
        })?;
        self.unix_utc_us
            .checked_add(offset_us)
            .ok_or(ApplicationError::ClockOverflow)
    }
}

struct SnapshotCaptureSchedule {
    observation_interval_s: Option<f64>,
    next_observation_s: f64,
    diagnostic_batches_remaining: u64,
}

impl SnapshotCaptureSchedule {
    fn new(observation_interval_s: Option<f64>, diagnostic_batches_remaining: u64) -> Self {
        Self {
            observation_interval_s,
            next_observation_s: 0.0,
            diagnostic_batches_remaining,
        }
    }

    fn next(&mut self, completed_at_s: f64) -> SnapshotCapture {
        let observation_due = self.observation_interval_s.is_some_and(|interval_s| {
            if completed_at_s + 1e-12 < self.next_observation_s {
                return false;
            }
            self.next_observation_s =
                (completed_at_s / interval_s + 1e-12).floor() * interval_s + interval_s;
            true
        });
        let diagnostics_due = self.diagnostic_batches_remaining > 0;
        self.diagnostic_batches_remaining = self.diagnostic_batches_remaining.saturating_sub(1);
        SnapshotCapture {
            required: observation_due || diagnostics_due,
            observation_due,
        }
    }
}

struct SnapshotCapture {
    required: bool,
    observation_due: bool,
}

struct GenerationCoordinator {
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<GeneratedOutput>,
    thread: Option<JoinHandle<std::result::Result<(), String>>>,
}

impl GenerationCoordinator {
    fn start(
        mut runtime: GenerationRuntime,
        epoch_monotonic_ns: u64,
        snapshot_schedule: SnapshotCaptureSchedule,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel(GENERATION_OUTPUT_CAPACITY);
        let worker = thread::Builder::new()
            .name("lidar-generation-coordinator".into())
            .spawn(move || {
                let result = run_generation_worker(
                    &mut runtime,
                    epoch_monotonic_ns,
                    snapshot_schedule,
                    &worker_stop,
                    &sender,
                );
                let shutdown = runtime.shutdown().map_err(|error| error.to_string());
                result.and(shutdown)
            })
            .map_err(ApplicationError::GenerationStart)?;
        Ok(Self {
            stop,
            receiver,
            thread: Some(worker),
        })
    }

    async fn next(&mut self) -> Option<GeneratedOutput> {
        self.receiver.recv().await
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.receiver.close();
        let Some(worker) = self.thread.take() else {
            return Ok(());
        };
        worker.thread().unpark();
        match tokio::task::spawn_blocking(move || worker.join())
            .await
            .map_err(ApplicationError::GenerationJoin)?
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(ApplicationError::GenerationWorker(error)),
            Err(_) => Err(ApplicationError::GenerationPanic),
        }
    }
}

impl Drop for GenerationCoordinator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.close();
        if let Some(worker) = self.thread.take() {
            worker.thread().unpark();
        }
    }
}

fn run_generation_worker(
    runtime: &mut GenerationRuntime,
    epoch_monotonic_ns: u64,
    mut snapshot_schedule: SnapshotCaptureSchedule,
    stop: &AtomicBool,
    sender: &mpsc::Sender<GeneratedOutput>,
) -> std::result::Result<(), String> {
    while !stop.load(Ordering::Acquire) {
        let next_s = runtime.next_completion_elapsed_s();
        let offset = Duration::try_from_secs_f64(next_s)
            .map_err(|_| ApplicationError::DeadlineOverflow.to_string())?;
        let offset_ns = u64::try_from(offset.as_nanos())
            .map_err(|_| ApplicationError::DeadlineOverflow.to_string())?;
        let deadline_monotonic_ns = epoch_monotonic_ns
            .checked_add(offset_ns)
            .ok_or_else(|| ApplicationError::DeadlineOverflow.to_string())?;
        while !stop.load(Ordering::Acquire) {
            let now = monotonic_time_ns().map_err(|error| error.to_string())?;
            if now >= deadline_monotonic_ns {
                break;
            }
            thread::park_timeout(Duration::from_nanos(deadline_monotonic_ns - now));
        }
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let batch = runtime
            .next_completed_scans()
            .map_err(|error| error.to_string())?;
        let capture = snapshot_schedule.next(batch.completed_at_s);
        let snapshot = capture
            .required
            .then(|| runtime.model_snapshot().map_err(|error| error.to_string()))
            .transpose()?;
        if sender
            .blocking_send(GeneratedOutput {
                batch,
                #[cfg(feature = "edge-validation")]
                deadline_monotonic_ns,
                snapshot,
                observation_due: capture.observation_due,
            })
            .is_err()
        {
            return if stop.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("generation output receiver stopped".to_owned())
            };
        }
    }
    Ok(())
}

enum ApplicationMode {
    Continuous,
    #[cfg(feature = "edge-validation")]
    EdgeValidation(EdgeValidationSettings),
}

impl ApplicationMode {
    fn publishes_observations(&self) -> bool {
        match self {
            Self::Continuous => true,
            #[cfg(feature = "edge-validation")]
            Self::EdgeValidation(settings) => {
                settings.observation_mode == EdgeValidationObservationMode::Actual
            }
        }
    }
}

enum ApplicationObservation {
    Actual(TcpObservationPublisher),
    #[cfg(feature = "edge-validation")]
    NoOp,
}

impl ApplicationObservation {
    fn endpoint(&self) -> String {
        match self {
            Self::Actual(publisher) => publisher.endpoint(),
            #[cfg(feature = "edge-validation")]
            Self::NoOp => "no-op".to_owned(),
        }
    }

    fn start(&mut self) -> std::result::Result<(), ObservationError> {
        match self {
            Self::Actual(publisher) => publisher.start(),
            #[cfg(feature = "edge-validation")]
            Self::NoOp => Ok(()),
        }
    }

    fn publish(
        &self,
        snapshot: crate::scenario::ScenarioModelSnapshot,
    ) -> std::result::Result<bool, ObservationError> {
        match self {
            Self::Actual(publisher) => publisher.publish(snapshot),
            #[cfg(feature = "edge-validation")]
            Self::NoOp => Err(ObservationError::Invalid(
                "edge validation no-op publisher received an observation snapshot",
            )),
        }
    }

    fn stats(&self) -> ObservationPublisherStats {
        match self {
            Self::Actual(publisher) => publisher.stats(),
            #[cfg(feature = "edge-validation")]
            Self::NoOp => ObservationPublisherStats::default(),
        }
    }

    async fn close(&mut self) {
        match self {
            Self::Actual(publisher) => publisher.close().await,
            #[cfg(feature = "edge-validation")]
            Self::NoOp => {}
        }
    }
}

enum RunBoundary {
    Continuous,
    #[cfg(feature = "edge-validation")]
    EdgeValidation(Box<EdgeValidationRecorder>),
}

impl RunBoundary {
    fn requires_observation_publish_success(&self) -> bool {
        match self {
            Self::Continuous => false,
            #[cfg(feature = "edge-validation")]
            Self::EdgeValidation(_) => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunExit {
    Signal,
    #[cfg(feature = "edge-validation")]
    EdgeValidationComplete,
}

struct ApplicationOutcome {
    summary: ApplicationSummary,
    #[cfg(feature = "edge-validation")]
    edge_validation_report: Option<EdgeValidationReport>,
}

pub async fn run_simulator_application(
    inputs: SimulatorInputs,
    settings: RuntimeSettings,
) -> Result<ApplicationSummary> {
    Ok(
        run_application(inputs, settings, ApplicationMode::Continuous)
            .await?
            .summary,
    )
}

#[cfg(feature = "edge-validation")]
pub async fn run_edge_validation_application(
    inputs: SimulatorInputs,
    settings: RuntimeSettings,
    validation: EdgeValidationSettings,
) -> Result<ApplicationSummary> {
    let output_path = validation.output_path.clone();
    let outcome = run_application(
        inputs,
        settings,
        ApplicationMode::EdgeValidation(validation),
    )
    .await?;
    let report = outcome
        .edge_validation_report
        .ok_or(EdgeValidationError::Incomplete)?;
    write_edge_validation_report_atomic(&output_path, &report)?;
    Ok(outcome.summary)
}

async fn run_application(
    inputs: SimulatorInputs,
    settings: RuntimeSettings,
    mode: ApplicationMode,
) -> Result<ApplicationOutcome> {
    let mut shutdown = ShutdownSignals::new()?;
    let generation = GenerationRuntime::from_inputs(&inputs)?;
    let sensor_ids: [String; 2] = inputs
        .environment
        .sensors
        .iter()
        .map(|sensor| sensor.sensor_id.clone())
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| GenerationRuntimeError::SensorSet)?;
    let run_id = Uuid::new_v4().to_string();
    let mut observation = if mode.publishes_observations() {
        let fingerprint = simulator_input_fingerprint(&inputs)?;
        let observation_config = ObservationPublisherConfig::from_transport(
            settings.observation_host.clone(),
            settings.observation_port,
            settings.observation_interval_s,
            &inputs.simulator.observation_transport,
        )?;
        let observation_header = ObservationStreamHeader::new(
            inputs.environment.environment_id.clone(),
            run_id.clone(),
            fingerprint,
            inputs.simulator.seed,
            ObservationScene::from_inputs(&inputs)?,
        )?;
        ApplicationObservation::Actual(TcpObservationPublisher::new(
            observation_config,
            observation_header,
        )?)
    } else {
        #[cfg(feature = "edge-validation")]
        {
            ApplicationObservation::NoOp
        }
        #[cfg(not(feature = "edge-validation"))]
        unreachable!("continuous execution always publishes observations")
    };
    let observation_endpoint = observation.endpoint();

    let scan_config = ScanRuntimeConfig {
        sensor_ids: sensor_ids.clone(),
        socket_directory: settings.grpc_socket_dir,
        status_directory: settings.status_dir,
        edge_id: settings.edge_id.clone(),
        config_revision: settings.config_revision.clone(),
        site_id: settings.site_id,
        deployment_revision: settings.deployment_revision,
        service_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let mut scan = GrpcScanRuntime::start_system(scan_config).await?;
    let mut frame_factory = match SystemScanFrameFactory::with_system_clocks(
        sensor_ids.clone(),
        settings.edge_id,
        settings.config_revision,
        scan.instance_ids(),
    ) {
        Ok(factory) => factory,
        Err(error) => {
            scan.close().await;
            return Err(error.into());
        }
    };
    if let Err(error) = observation.start() {
        scan.close().await;
        return Err(error.into());
    }

    // Capture one wall/monotonic clock pair after scan and observation outputs are ready.
    // Diagnostics and generation deadlines derive from the same selected simulation epoch,
    // including a requested future edge-validation start.
    let clock_anchor = match system_clock_anchor() {
        Ok(anchor) => anchor,
        Err(error) => {
            observation.close().await;
            scan.close().await;
            return Err(error);
        }
    };
    let generation_epoch_monotonic_ns = match &mode {
        ApplicationMode::Continuous => clock_anchor.monotonic_ns,
        #[cfg(feature = "edge-validation")]
        ApplicationMode::EdgeValidation(validation) => {
            match validation.generation_epoch_monotonic_ns(clock_anchor.monotonic_ns) {
                Ok(epoch) => epoch,
                Err(error) => {
                    observation.close().await;
                    scan.close().await;
                    return Err(error.into());
                }
            }
        }
    };
    let run_started_at_utc_us = match clock_anchor.unix_utc_us_at(generation_epoch_monotonic_ns) {
        Ok(timestamp) => timestamp,
        Err(error) => {
            observation.close().await;
            scan.close().await;
            return Err(error);
        }
    };
    let mut diagnostics = if inputs.simulator.diagnostics.enabled {
        match DiagnosticsWriterConfig::from_inputs(&inputs, run_id.clone(), run_started_at_utc_us)
            .and_then(BoundedDiagnosticsWriter::start)
        {
            Ok(writer) => Some(writer),
            Err(error) => {
                observation.close().await;
                scan.close().await;
                return Err(error.into());
            }
        }
    } else {
        None
    };

    let diagnostic_batches = if diagnostics.is_some() {
        inputs
            .simulator
            .diagnostics
            .sample_scan_limit_per_sensor
            .get()
    } else {
        0
    };
    let observation_interval = mode
        .publishes_observations()
        .then_some(settings.observation_interval_s);
    let snapshot_schedule = SnapshotCaptureSchedule::new(observation_interval, diagnostic_batches);
    let boundary_result: Result<RunBoundary> = match &mode {
        ApplicationMode::Continuous => Ok(RunBoundary::Continuous),
        #[cfg(feature = "edge-validation")]
        ApplicationMode::EdgeValidation(validation) => EdgeValidationRecorder::new(
            validation,
            generation_epoch_monotonic_ns,
            sensor_ids.clone(),
            inputs.simulator.measurement.rotation_rate_hz,
        )
        .map(Box::new)
        .map(RunBoundary::EdgeValidation)
        .map_err(ApplicationError::from),
    };
    let mut boundary = match boundary_result {
        Ok(boundary) => boundary,
        Err(error) => {
            observation.close().await;
            if let Some(writer) = &mut diagnostics {
                writer.close().await;
            }
            scan.close().await;
            return Err(error);
        }
    };
    #[cfg(feature = "edge-validation")]
    if let ApplicationMode::EdgeValidation(validation) = &mode
        && validation.start_at_monotonic_ns.is_some()
    {
        let current = match monotonic_time_ns() {
            Ok(current) => current,
            Err(error) => {
                observation.close().await;
                if let Some(writer) = &mut diagnostics {
                    writer.close().await;
                }
                scan.close().await;
                return Err(error);
            }
        };
        if let Err(error) = validation.generation_epoch_monotonic_ns(current) {
            observation.close().await;
            if let Some(writer) = &mut diagnostics {
                writer.close().await;
            }
            scan.close().await;
            return Err(error.into());
        }
    }
    let mut generation = match GenerationCoordinator::start(
        generation,
        generation_epoch_monotonic_ns,
        snapshot_schedule,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            observation.close().await;
            if let Some(writer) = &mut diagnostics {
                writer.close().await;
            }
            scan.close().await;
            return Err(error);
        }
    };
    let mut fatal = scan.fatal_receiver();
    let mut generated_scans = 0_u64;
    let mut published_scans = 0_u64;
    let execution = run_until_shutdown(
        &mut generation,
        &mut shutdown,
        &mut frame_factory,
        &scan,
        &mut fatal,
        &mut observation,
        &mut diagnostics,
        &mut generated_scans,
        &mut published_scans,
        &mut boundary,
    )
    .await;

    let generation_shutdown = generation.shutdown().await;
    #[cfg(feature = "edge-validation")]
    if matches!(&execution, Ok(RunExit::EdgeValidationComplete)) && generation_shutdown.is_ok() {
        // Keep the process and status outputs readable past the final scan deadline so the
        // independent one-second cgroup sampler can capture its last interval without racing
        // process teardown. Generation is already stopped, so the hold adds no measured work.
        tokio::time::sleep(EDGE_VALIDATION_POST_MEASUREMENT_HOLD).await;
    }
    observation.close().await;
    if let Some(writer) = &mut diagnostics {
        writer.close().await;
    }
    scan.close().await;
    let scan_stats = scan.stats();

    #[cfg(feature = "edge-validation")]
    let exit = resolve_execution_result(execution, generation_shutdown)?;
    #[cfg(not(feature = "edge-validation"))]
    resolve_execution_result(execution, generation_shutdown)?;
    debug_assert_eq!(published_scans, total_published(&scan_stats));
    let observation_stats = observation.stats();
    let summary = ApplicationSummary {
        run_id,
        run_started_at_utc_us,
        generated_scans,
        scan_stats,
        observation_endpoint,
        observation_stats,
    };
    #[cfg(feature = "edge-validation")]
    let edge_validation_report = match boundary {
        RunBoundary::Continuous => None,
        RunBoundary::EdgeValidation(recorder) => {
            if exit != RunExit::EdgeValidationComplete {
                return Err(EdgeValidationError::Interrupted.into());
            }
            Some((*recorder).finish(
                summary.run_id.clone(),
                summary.generated_scans,
                &summary.scan_stats,
                summary.observation_stats,
            )?)
        }
    };
    Ok(ApplicationOutcome {
        summary,
        #[cfg(feature = "edge-validation")]
        edge_validation_report,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_until_shutdown(
    generation: &mut GenerationCoordinator,
    shutdown: &mut ShutdownSignals,
    frame_factory: &mut SystemScanFrameFactory,
    scan: &GrpcScanRuntime,
    fatal: &mut watch::Receiver<Option<String>>,
    observation: &mut ApplicationObservation,
    diagnostics: &mut Option<BoundedDiagnosticsWriter>,
    generated_scans: &mut u64,
    published_scans: &mut u64,
    boundary: &mut RunBoundary,
) -> Result<RunExit> {
    #[cfg(not(feature = "edge-validation"))]
    let _ = &boundary;
    loop {
        if let Some(error) = fatal.borrow().clone() {
            return Err(ApplicationError::ScanFatal(error));
        }
        tokio::select! {
            biased;
            signal = shutdown.wait() => {
                signal?;
                return Ok(RunExit::Signal);
            }
            changed = fatal.changed() => {
                if changed.is_err() {
                    return Err(ApplicationError::ScanFatal(
                        "scan fatal channel stopped".to_owned(),
                    ));
                }
            }
            output = generation.next() => {
                let Some(mut output) = output else {
                    return Err(ApplicationError::GenerationStopped);
                };
                let scan_count = u64::try_from(output.batch.scans.len())
                    .map_err(|_| GenerationRuntimeError::SensorConfiguration)?;
                #[cfg(feature = "edge-validation")]
                let validation_batch = matches!(&*boundary, RunBoundary::EdgeValidation(_));
                #[cfg(feature = "edge-validation")]
                let measured_batch = matches!(
                    &*boundary,
                    RunBoundary::EdgeValidation(recorder)
                        if recorder.measures_deadline(output.deadline_monotonic_ns)
                );
                #[cfg(feature = "edge-validation")]
                if measured_batch
                    && let RunBoundary::EdgeValidation(recorder) = &mut *boundary
                {
                    recorder.observe_subscribers(&scan.stats())?;
                    recorder.record_scan_semantics(
                        output.deadline_monotonic_ns,
                        &output.batch.scans,
                    )?;
                }
                #[cfg(feature = "edge-validation")]
                let mut frame_timings = validation_batch.then(|| Vec::with_capacity(2));
                for result in output.batch.scans {
                    if let Some(writer) = diagnostics
                        && writer.wants_record(result.sensor_id())?
                    {
                        let snapshot = output.snapshot.clone().ok_or_else(|| {
                            ApplicationError::GenerationWorker(
                                "diagnostic snapshot was not captured".to_owned(),
                            )
                        })?;
                        let record = DiagnosticsRecordInput::from_measurement(
                            &result,
                            snapshot,
                        )?;
                        writer.try_record(record)?;
                    }
                    if let Some(frame) = frame_factory.build(result)? {
                        #[cfg(feature = "edge-validation")]
                        let timing_identity = if validation_batch {
                            let RunBoundary::EdgeValidation(recorder) = &*boundary else {
                                unreachable!("validation batches require an edge validation boundary");
                            };
                            Some((recorder.sensor_index(&frame.sensor_id)?, frame.sequence))
                        } else {
                            None
                        };
                        let publish_receipt = scan.publish(frame).await?;
                        *published_scans = published_scans.saturating_add(1);
                        #[cfg(not(feature = "edge-validation"))]
                        let _ = publish_receipt;
                        #[cfg(feature = "edge-validation")]
                        if let Some((sensor_index, sequence)) = timing_identity {
                            frame_timings
                                .as_mut()
                                .expect("measured frame timing storage was allocated")
                                .push(PublishedFrameTiming {
                                sensor_index,
                                sequence,
                                published_monotonic_ns: publish_receipt.published_at_monotonic_ns,
                            });
                        }
                    }
                }
                *generated_scans = generated_scans.saturating_add(scan_count);
                if output.observation_due {
                    let snapshot = output.snapshot.take().ok_or_else(|| {
                        ApplicationError::GenerationWorker(
                            "due observation snapshot was not captured".to_owned(),
                        )
                    })?;
                    resolve_observation_publish(
                        boundary,
                        observation.publish(snapshot),
                    )?;
                }
                #[cfg(feature = "edge-validation")]
                if let RunBoundary::EdgeValidation(recorder) = &mut *boundary
                    && recorder.record_batch(
                        output.deadline_monotonic_ns,
                        frame_timings.as_deref().unwrap_or_default(),
                        &output.batch.scenario_transitions,
                    )?
                {
                    return Ok(RunExit::EdgeValidationComplete);
                }
            }
        }
    }
}

fn resolve_observation_publish(
    boundary: &RunBoundary,
    result: std::result::Result<bool, ObservationError>,
) -> Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if boundary.requires_observation_publish_success() => Err(error.into()),
        Err(_) => Ok(()),
    }
}

struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: Signal,
    #[cfg(unix)]
    terminate: Signal,
}

impl ShutdownSignals {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                interrupt: tokio::signal::unix::signal(SignalKind::interrupt())
                    .map_err(ApplicationError::Signal)?,
                terminate: tokio::signal::unix::signal(SignalKind::terminate())
                    .map_err(ApplicationError::Signal)?,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    async fn wait(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            tokio::select! {
                received = self.interrupt.recv() => signal_received(received, "SIGINT"),
                received = self.terminate.recv() => signal_received(received, "SIGTERM"),
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c()
            .await
            .map_err(ApplicationError::Signal)
    }
}

#[cfg(unix)]
fn signal_received(received: Option<()>, name: &'static str) -> Result<()> {
    if received.is_some() {
        Ok(())
    } else {
        Err(ApplicationError::Signal(std::io::Error::other(format!(
            "{name} signal stream stopped"
        ))))
    }
}

fn unix_time_us() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApplicationError::ClockBeforeEpoch)?;
    i64::try_from(elapsed.as_micros()).map_err(|_| ApplicationError::ClockOverflow)
}

fn system_clock_anchor() -> Result<ClockAnchor> {
    let monotonic_before_ns = monotonic_time_ns()?;
    let unix_utc_us = unix_time_us()?;
    let monotonic_after_ns = monotonic_time_ns()?;
    let elapsed_ns = monotonic_after_ns.checked_sub(monotonic_before_ns).ok_or(
        ApplicationError::ClockMapping("host monotonic clock moved backwards"),
    )?;
    let monotonic_ns =
        monotonic_before_ns
            .checked_add(elapsed_ns / 2)
            .ok_or(ApplicationError::ClockMapping(
                "host monotonic clock midpoint exceeds u64",
            ))?;
    Ok(ClockAnchor {
        monotonic_ns,
        unix_utc_us,
    })
}

fn monotonic_time_ns() -> Result<u64> {
    let monotonic = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(monotonic.tv_sec).map_err(|_| {
        ApplicationError::Scan(ScanRuntimeError::Clock(
            "host monotonic seconds are negative",
        ))
    })?;
    let nanoseconds = u64::try_from(monotonic.tv_nsec).map_err(|_| {
        ApplicationError::Scan(ScanRuntimeError::Clock(
            "host monotonic nanoseconds are negative",
        ))
    })?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .ok_or(ApplicationError::Scan(ScanRuntimeError::Clock(
            "host monotonic nanoseconds exceed u64",
        )))
}

fn total_published(stats: &ScanRuntimeStats) -> u64 {
    stats
        .sensors
        .iter()
        .map(|sensor| sensor.published_frames)
        .sum()
}

fn resolve_execution_result(execution: Result<RunExit>, shutdown: Result<()>) -> Result<RunExit> {
    match (execution, shutdown) {
        (Err(ApplicationError::GenerationStopped), Err(error)) => Err(error),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(exit), Ok(())) => Ok(exit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_failure_has_priority_over_closed_output_channel() {
        let error = resolve_execution_result(
            Err(ApplicationError::GenerationStopped),
            Err(ApplicationError::GenerationWorker(
                "worker failed".to_owned(),
            )),
        )
        .unwrap_err();
        assert!(matches!(error, ApplicationError::GenerationWorker(_)));
    }

    #[test]
    fn primary_execution_failure_has_priority_over_shutdown_failure() {
        let error = resolve_execution_result(
            Err(ApplicationError::ScanFatal("scan failed".to_owned())),
            Err(ApplicationError::GenerationWorker(
                "worker failed".to_owned(),
            )),
        )
        .unwrap_err();
        assert!(matches!(error, ApplicationError::ScanFatal(_)));
    }

    #[test]
    fn continuous_execution_isolates_observation_publish_errors() {
        let result =
            resolve_observation_publish(&RunBoundary::Continuous, Err(ObservationError::Closed));

        assert!(result.is_ok());
    }

    #[test]
    fn snapshot_capture_stops_after_diagnostics_and_follows_observation_cadence() {
        let mut schedule = SnapshotCaptureSchedule::new(Some(1.0), 2);

        let first = schedule.next(0.1);
        assert!(first.required);
        assert!(first.observation_due);
        let second = schedule.next(0.2);
        assert!(second.required);
        assert!(!second.observation_due);
        let third = schedule.next(0.3);
        assert!(!third.required);
        assert!(!third.observation_due);
        let tenth = schedule.next(1.0);
        assert!(tenth.required);
        assert!(tenth.observation_due);
    }

    #[test]
    fn clock_anchor_maps_continuous_and_future_generation_epochs() {
        let anchor = ClockAnchor {
            monotonic_ns: 10_000,
            unix_utc_us: 1_800_000_000_000_000,
        };

        assert_eq!(
            anchor.unix_utc_us_at(10_000).unwrap(),
            1_800_000_000_000_000
        );
        assert_eq!(
            anchor.unix_utc_us_at(1_000_010_499).unwrap(),
            1_800_000_001_000_000
        );
        assert_eq!(
            anchor.unix_utc_us_at(1_000_010_500).unwrap(),
            1_800_000_001_000_001
        );
    }

    #[test]
    fn clock_anchor_rejects_an_epoch_before_its_monotonic_origin() {
        let error = ClockAnchor {
            monotonic_ns: 10,
            unix_utc_us: 20,
        }
        .unix_utc_us_at(9)
        .unwrap_err();

        assert!(matches!(error, ApplicationError::ClockMapping(_)));
    }

    #[cfg(feature = "edge-validation")]
    #[test]
    fn no_op_validation_never_requests_observation_snapshots() {
        let mut schedule = SnapshotCaptureSchedule::new(None, 0);
        for completed_at_s in [0.1, 0.2, 1.0, 10.0] {
            let capture = schedule.next(completed_at_s);
            assert!(!capture.required);
            assert!(!capture.observation_due);
        }
    }
}

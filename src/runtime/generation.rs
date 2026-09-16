//! Time-ordered scenario coordination with one fixed worker per sensor.

use std::{
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

use crate::{
    configuration::{SensorConfig, SimulatorInputs},
    geometry::{GeometryError, Polygon2, Vec2, Vec3},
    measurement::{
        CollectionOcclusionSettings, DistortionInterval, DropoutSettings, EnvironmentScene,
        FallingMaterialSettings, MeasurementError, MeasurementGenerator, MeasurementResult,
        MeasurementSettings, QualityDistribution, ReferenceScanner, ScheduledScan, SensorFrame,
        SensorRotationScheduler, SnapshotEventCoordinator, SpatialDistortionResolver,
        SpatialDistortionTimeline, VoidSettings, create_seeded_rotation_scheduler,
    },
    scenario::{
        ScenarioError, ScenarioModelSnapshot, ScenarioPhase, ScenarioSimulator,
        build_scenario_simulator, scale_duration_range, scenario_time_scale,
    },
};

const SENSOR_WORKER_QUEUE_CAPACITY: usize = 1;
const MAX_RETAINED_SPATIAL_EVENTS: usize = 100_000;

#[derive(Debug, thiserror::Error)]
pub enum GenerationRuntimeError {
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error(transparent)]
    Measurement(#[from] MeasurementError),
    #[error(transparent)]
    Rotation(#[from] crate::measurement::rotation::RotationError),
    #[error(transparent)]
    Scenario(#[from] ScenarioError),
    #[error("sensor worker could not start: {0}")]
    WorkerStart(#[source] std::io::Error),
    #[error("sensor worker stopped before accepting its scan")]
    WorkerRequest,
    #[error("sensor worker stopped before returning its scan")]
    WorkerResponse,
    #[error("sensor worker panicked")]
    WorkerPanic,
    #[error("sensor worker returned a response for a different scan")]
    WorkerResponseMismatch,
    #[error("generation runtime cannot be reused after a terminal failure or shutdown")]
    Terminal,
    #[error("generation runtime requires exactly two unique sensors")]
    SensorSet,
    #[error("generation runtime sensor configuration is inconsistent")]
    SensorConfiguration,
    #[error("scaled spatial event rate is outside the finite range")]
    EventRate,
}

pub type Result<T> = std::result::Result<T, GenerationRuntimeError>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GenerationRuntimeStats {
    pub completed_batches: u64,
    pub generated_scans: u64,
}

#[derive(Debug)]
pub struct GenerationBatch {
    pub completed_at_s: f64,
    pub scans: Vec<MeasurementResult>,
    pub scenario_transitions: Vec<ScenarioPhaseTransition>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenarioPhaseTransition {
    pub elapsed_s: f64,
    pub cycle_index: u64,
    pub phase: ScenarioPhase,
}

struct SensorCoordinator {
    sensor_id: String,
    scheduler: SensorRotationScheduler,
    pending: Option<ScheduledScan>,
    request: SyncSender<WorkerCommand>,
    response: Receiver<WorkerResponse>,
    worker: Option<JoinHandle<()>>,
}

struct WorkerResponse {
    scan_id: u64,
    completed_at_bits: u64,
    result: std::result::Result<MeasurementResult, MeasurementError>,
}

#[derive(Clone, Copy)]
struct DispatchedScan {
    sensor_index: usize,
    scan_id: u64,
    completed_at_bits: u64,
}

enum WorkerCommand {
    Generate {
        schedule: ScheduledScan,
        snapshots: Arc<SnapshotEventCoordinator>,
        spatial: Option<Arc<SpatialDistortionResolver>>,
    },
    Stop,
}

pub struct GenerationRuntime {
    scenario: ScenarioSimulator,
    snapshots: SnapshotEventCoordinator,
    spatial: Option<SpatialDistortionTimeline>,
    sensors: Vec<SensorCoordinator>,
    stats: GenerationRuntimeStats,
    terminal: bool,
    shutdown: bool,
}

impl GenerationRuntime {
    pub fn from_inputs(inputs: &SimulatorInputs) -> Result<Self> {
        if inputs.environment.sensors.len() != 2
            || inputs.environment.sensors[0].sensor_id == inputs.environment.sensors[1].sensor_id
        {
            return Err(GenerationRuntimeError::SensorSet);
        }

        let scenario = build_scenario_simulator(inputs)?;
        let initial_surface = scenario.surface().surface_snapshot()?;
        let snapshots = SnapshotEventCoordinator::new(initial_surface);
        let scene = Arc::new(build_environment_scene(inputs)?);
        let spatial = build_spatial_timeline(inputs, scene.as_ref().clone())?;
        let mut sensors = Vec::with_capacity(2);
        for (ordinal, sensor) in inputs.environment.sensors.iter().enumerate() {
            match start_sensor_worker(inputs, sensor, ordinal, Arc::clone(&scene), &snapshots) {
                Ok(worker) => sensors.push(worker),
                Err(error) => {
                    shutdown_sensor_workers(&mut sensors);
                    return Err(error);
                }
            }
        }

        Ok(Self {
            scenario,
            snapshots,
            spatial,
            sensors,
            stats: GenerationRuntimeStats::default(),
            terminal: false,
            shutdown: false,
        })
    }

    pub fn sensor_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.sensors.iter().map(|sensor| sensor.sensor_id.as_str())
    }

    pub fn next_completion_elapsed_s(&self) -> f64 {
        self.sensors
            .iter()
            .filter_map(|sensor| sensor.pending.as_ref())
            .map(ScheduledScan::completed_at_s)
            .fold(f64::INFINITY, f64::min)
    }

    pub fn earliest_pending_elapsed_s(&self) -> f64 {
        self.sensors
            .iter()
            .filter_map(|sensor| sensor.pending.as_ref())
            .map(ScheduledScan::captured_elapsed_s)
            .fold(f64::INFINITY, f64::min)
    }

    pub fn elapsed_s(&self) -> f64 {
        self.scenario.elapsed_s()
    }

    pub fn model_snapshot(&self) -> Result<ScenarioModelSnapshot> {
        Ok(self.scenario.observation_snapshot()?)
    }

    pub fn stats(&self) -> GenerationRuntimeStats {
        self.stats
    }

    pub fn next_completed_scans(&mut self) -> Result<GenerationBatch> {
        if self.terminal || self.shutdown {
            return Err(GenerationRuntimeError::Terminal);
        }
        let result = self.try_next_completed_scans();
        if result.is_err() {
            self.terminal = true;
        }
        result
    }

    fn try_next_completed_scans(&mut self) -> Result<GenerationBatch> {
        let completion_s = self.next_completion_elapsed_s();
        if !completion_s.is_finite() {
            return Err(GenerationRuntimeError::SensorConfiguration);
        }
        let scenario_transitions = self.advance_scenario_and_distortions(completion_s)?;

        let snapshots = Arc::new(self.snapshots.clone());
        let spatial = self
            .spatial
            .as_ref()
            .map(|timeline| Arc::new(timeline.resolver().clone()));
        let requested_indices: Vec<_> = self
            .sensors
            .iter()
            .enumerate()
            .filter_map(|(index, sensor)| {
                sensor
                    .pending
                    .as_ref()
                    .filter(|pending| pending.completed_at_s().to_bits() == completion_s.to_bits())
                    .map(|_| index)
            })
            .collect();
        if requested_indices.is_empty()
            || self.sensors.iter().any(|sensor| sensor.pending.is_none())
        {
            return Err(GenerationRuntimeError::SensorConfiguration);
        }

        // Dispatch every due scan before calculating any following schedule so both
        // fixed workers can start the CPU-heavy ray casting at the same time.
        let mut dispatched = Vec::with_capacity(requested_indices.len());
        let mut first_error = None;
        for &index in &requested_indices {
            let sensor = &mut self.sensors[index];
            let schedule = sensor
                .pending
                .take()
                .expect("pending schedules were validated above");
            let metadata = DispatchedScan {
                sensor_index: index,
                scan_id: schedule.scan_id(),
                completed_at_bits: schedule.completed_at_s().to_bits(),
            };
            if sensor
                .request
                .send(WorkerCommand::Generate {
                    schedule,
                    snapshots: Arc::clone(&snapshots),
                    spatial: spatial.as_ref().map(Arc::clone),
                })
                .is_err()
            {
                record_first_error(&mut first_error, GenerationRuntimeError::WorkerRequest);
            } else {
                dispatched.push(metadata);
            }
        }

        // Scheduling is independent of worker execution and overlaps it. Any
        // scheduler failure is terminal, but all in-flight responses are still
        // drained below before the error is returned.
        for dispatched_scan in &dispatched {
            let sensor = &mut self.sensors[dispatched_scan.sensor_index];
            match sensor.scheduler.next_scan() {
                Ok(next) => sensor.pending = Some(next),
                Err(error) => record_first_error(&mut first_error, error.into()),
            }
        }

        let mut scans = Vec::with_capacity(dispatched.len());
        for expected in dispatched {
            match receive_worker_response(&mut self.sensors[expected.sensor_index]) {
                Ok(response)
                    if response.scan_id == expected.scan_id
                        && response.completed_at_bits == expected.completed_at_bits =>
                {
                    match response.result {
                        Ok(result)
                            if result.sensor_id()
                                == self.sensors[expected.sensor_index].sensor_id
                                && result.scan_id() == expected.scan_id
                                && result.reference().completed_at_s().to_bits()
                                    == expected.completed_at_bits =>
                        {
                            scans.push(result);
                        }
                        Ok(_) => record_first_error(
                            &mut first_error,
                            GenerationRuntimeError::WorkerResponseMismatch,
                        ),
                        Err(error) => record_first_error(&mut first_error, error.into()),
                    }
                }
                Ok(_) => record_first_error(
                    &mut first_error,
                    GenerationRuntimeError::WorkerResponseMismatch,
                ),
                Err(error) => record_first_error(&mut first_error, error),
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        if scans.is_empty() {
            return Err(GenerationRuntimeError::SensorConfiguration);
        }

        let earliest_pending_s = self.earliest_pending_elapsed_s();
        self.snapshots
            .discard_before_earliest_pending(earliest_pending_s)?;
        if let Some(spatial) = &mut self.spatial {
            spatial.discard_before(earliest_pending_s)?;
        }
        self.stats.completed_batches = self.stats.completed_batches.saturating_add(1);
        self.stats.generated_scans = self
            .stats
            .generated_scans
            .saturating_add(scans.len() as u64);
        Ok(GenerationBatch {
            completed_at_s: completion_s,
            scans,
            scenario_transitions,
        })
    }

    fn advance_scenario_and_distortions(
        &mut self,
        through_s: f64,
    ) -> Result<Vec<ScenarioPhaseTransition>> {
        let mut scenario = self.scenario.clone();
        let mut snapshots = self.snapshots.clone();
        let mut spatial = self.spatial.clone();
        let scenario_event_limit = scenario.event_output_limit()?;
        let mut scenario_events = 0_usize;
        let mut transitions = Vec::new();

        while scenario.elapsed_s() < through_s {
            let started_at_s = scenario.elapsed_s();
            let ends_at_s = scenario.next_surface_event_elapsed_s()?.min(through_s);
            if let Some(spatial) = &mut spatial {
                let surface = scenario.surface().surface_snapshot()?;
                spatial.advance_to(DistortionInterval {
                    started_at_s,
                    ends_at_s,
                    phase: scenario.active_phase()?.into(),
                    surface: &surface,
                })?;
                if spatial.resolver().retained_event_count() > MAX_RETAINED_SPATIAL_EVENTS {
                    return Err(MeasurementError::Exhausted(
                        "retained spatial event count exceeds the generation runtime limit",
                    )
                    .into());
                }
            }
            let advance = scenario.advance_to(ends_at_s)?;
            scenario_events = scenario_events.saturating_add(advance.events.len());
            if scenario_events > scenario_event_limit {
                return Err(ScenarioError::Resource(
                    "scenario advance exceeds the event output limit",
                )
                .into());
            }
            for event in advance.events {
                if event.phase_transition_due {
                    transitions.push(ScenarioPhaseTransition {
                        elapsed_s: event.elapsed_s,
                        cycle_index: event.model.state.cycle_index,
                        phase: event.model.state.phase,
                    });
                }
                snapshots.push_event(event.elapsed_s, event.model.surface)?;
            }
        }

        self.scenario = scenario;
        self.snapshots = snapshots;
        self.spatial = spatial;
        Ok(transitions)
    }

    pub fn shutdown(&mut self) -> Result<()> {
        if self.shutdown {
            return Ok(());
        }
        self.terminal = true;
        self.shutdown = true;
        let mut first_error = None;
        for sensor in &self.sensors {
            if sensor.worker.is_some() && sensor.request.send(WorkerCommand::Stop).is_err() {
                record_first_error(&mut first_error, GenerationRuntimeError::WorkerRequest);
            }
        }
        for sensor in &mut self.sensors {
            let Some(worker) = sensor.worker.take() else {
                continue;
            };
            if worker.join().is_err() {
                first_error = Some(GenerationRuntimeError::WorkerPanic);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for GenerationRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn start_sensor_worker(
    inputs: &SimulatorInputs,
    sensor: &SensorConfig,
    ordinal: usize,
    scene: Arc<EnvironmentScene>,
    snapshots: &SnapshotEventCoordinator,
) -> Result<SensorCoordinator> {
    let measurement = &inputs.simulator.measurement;
    let mut scheduler = create_seeded_rotation_scheduler(
        sensor.sensor_id.clone(),
        measurement.sample_rate_hz,
        measurement.rotation_rate_hz,
        inputs.simulator.seed,
    )?;
    let pending = scheduler.next_scan()?;
    let mut scanner = ReferenceScanner::new(
        sensor.sensor_id.clone(),
        sensor_frame(sensor)?,
        measurement.min_distance_m,
        measurement.max_distance_m,
    )?;
    // Warm every angle used by the first rotation before the real-time epoch starts.
    scanner.generate_with_snapshots(scene.as_ref(), snapshots, pending.clone())?;
    let mut generator = measurement_generator(inputs, &sensor.sensor_id)?;
    let (request, requests) = sync_channel(SENSOR_WORKER_QUEUE_CAPACITY);
    let (responses, response) = sync_channel(SENSOR_WORKER_QUEUE_CAPACITY);
    let sensor_id = sensor.sensor_id.clone();
    let worker_name = format!("lidar-sensor-{}", ordinal + 1);
    let worker = thread::Builder::new()
        .name(worker_name)
        .spawn(move || {
            while let Ok(command) = requests.recv() {
                let WorkerCommand::Generate {
                    schedule,
                    snapshots,
                    spatial,
                } = command
                else {
                    return;
                };
                let scan_id = schedule.scan_id();
                let completed_at_bits = schedule.completed_at_s().to_bits();
                let result = scanner
                    .generate_with_snapshots(scene.as_ref(), snapshots.as_ref(), schedule)
                    .and_then(|reference| generator.generate(reference, spatial.as_deref()));
                if responses
                    .send(WorkerResponse {
                        scan_id,
                        completed_at_bits,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .map_err(GenerationRuntimeError::WorkerStart)?;
    Ok(SensorCoordinator {
        sensor_id,
        scheduler,
        pending: Some(pending),
        request,
        response,
        worker: Some(worker),
    })
}

fn receive_worker_response(sensor: &mut SensorCoordinator) -> Result<WorkerResponse> {
    match sensor.response.recv() {
        Ok(response) => Ok(response),
        Err(_) => {
            let panicked = sensor
                .worker
                .take()
                .is_some_and(|worker| worker.join().is_err());
            if panicked {
                Err(GenerationRuntimeError::WorkerPanic)
            } else {
                Err(GenerationRuntimeError::WorkerResponse)
            }
        }
    }
}

fn record_first_error(slot: &mut Option<GenerationRuntimeError>, error: GenerationRuntimeError) {
    if slot.is_none() {
        *slot = Some(error);
    }
}

fn shutdown_sensor_workers(sensors: &mut [SensorCoordinator]) {
    for sensor in sensors.iter() {
        let _ = sensor.request.send(WorkerCommand::Stop);
    }
    for sensor in sensors {
        if let Some(worker) = sensor.worker.take() {
            let _ = worker.join();
        }
    }
}

fn build_environment_scene(inputs: &SimulatorInputs) -> Result<EnvironmentScene> {
    Ok(EnvironmentScene::new(
        boundary(inputs)?,
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        Vec::new(),
    )?)
}

fn sensor_frame(sensor: &SensorConfig) -> Result<SensorFrame> {
    Ok(SensorFrame::new(
        Vec3::try_from(sensor.p0_m)?,
        Vec3::try_from(sensor.u0)?,
        Vec3::try_from(sensor.u90)?,
    )?)
}

fn measurement_generator(
    inputs: &SimulatorInputs,
    sensor_id: &str,
) -> Result<MeasurementGenerator> {
    let measurement = &inputs.simulator.measurement;
    let quality = inputs
        .quality_profile
        .sensors
        .iter()
        .find(|quality| quality.sensor_id == sensor_id)
        .ok_or(GenerationRuntimeError::SensorConfiguration)?;
    let time_scale = scenario_time_scale(inputs.simulator.scenario.mean_fill_duration_s)?;
    let dropout = &measurement.distortions.dropout;
    Ok(MeasurementGenerator::new(
        MeasurementSettings {
            sensor_id: sensor_id.to_owned(),
            min_distance_m: measurement.min_distance_m,
            max_distance_m: measurement.max_distance_m,
            noise_enabled: measurement.distance_noise.enabled,
            noise_standard_deviation_m: measurement.distance_noise.standard_deviation_m,
            noise_limit_m: measurement.distance_noise.limit_m,
            reflection_error_enabled: measurement.distortions.reflection_error.enabled,
            reflection_error_probability: measurement.distortions.reflection_error.probability,
            reflection_error_reduction_range_m: measurement
                .distortions
                .reflection_error
                .distance_reduction_m_range,
            valid_quality: QualityDistribution::new(quality.valid_distance_frequencies)?,
            invalid_quality: QualityDistribution::new(quality.invalid_distance_frequencies)?,
            dropout: dropout
                .enabled
                .then(|| -> Result<DropoutSettings> {
                    Ok(DropoutSettings {
                        event_interval_s_range: scale_duration_range(
                            dropout.event_interval_s_range,
                            time_scale,
                        )?,
                        duration_s_range: scale_duration_range(
                            dropout.duration_s_range,
                            time_scale,
                        )?,
                    })
                })
                .transpose()?,
        },
        inputs.simulator.seed,
    )?)
}

fn build_spatial_timeline(
    inputs: &SimulatorInputs,
    static_scene: EnvironmentScene,
) -> Result<Option<SpatialDistortionTimeline>> {
    let simulator = &inputs.simulator;
    let distortions = &simulator.measurement.distortions;
    if !distortions.falling_material.enabled
        && !distortions.voids.enabled
        && !distortions.collection_occlusion.enabled
    {
        return Ok(None);
    }
    let time_scale = scenario_time_scale(simulator.scenario.mean_fill_duration_s)?;
    let falling = &distortions.falling_material;
    let voids = &distortions.voids;
    let collection = &distortions.collection_occlusion;
    let timeline = SpatialDistortionTimeline::new(
        boundary(inputs)?,
        static_scene,
        falling
            .enabled
            .then(|| -> Result<FallingMaterialSettings> {
                let rate = falling.event_rate_per_s / time_scale;
                if !rate.is_finite() || rate < 0.0 {
                    return Err(GenerationRuntimeError::EventRate);
                }
                Ok(FallingMaterialSettings {
                    event_rate_per_s: rate,
                    radius_m_range: falling.radius_m_range,
                    duration_s_range: scale_duration_range(falling.duration_s_range, time_scale)?,
                    distance_reduction_m_range: falling.distance_reduction_m_range,
                    inlet_positions: simulator
                        .scenario
                        .inlet_positions_xy_m
                        .iter()
                        .copied()
                        .map(Vec2::try_from)
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                    placement_radius_m: simulator.scenario.surface.pile_spread_radius_m,
                })
            })
            .transpose()?,
        voids
            .enabled
            .then(|| -> Result<VoidSettings> {
                Ok(VoidSettings {
                    surface_area_ratio: voids.surface_area_ratio,
                    radius_m_range: voids.radius_m_range,
                    duration_s_range: scale_duration_range(voids.duration_s_range, time_scale)?,
                    cover_height_increase_m: voids.cover_height_increase_m,
                    distance_increase_m_range: voids.distance_increase_m_range,
                })
            })
            .transpose()?,
        collection
            .enabled
            .then(|| -> Result<CollectionOcclusionSettings> {
                Ok(CollectionOcclusionSettings {
                    event_interval_s_range: scale_duration_range(
                        collection.event_interval_s_range,
                        time_scale,
                    )?,
                    radius_m_range: collection.radius_m_range,
                    duration_s_range: scale_duration_range(
                        collection.duration_s_range,
                        time_scale,
                    )?,
                    distance_reduction_m_range: collection.distance_reduction_m_range,
                })
            })
            .transpose()?,
        simulator.seed,
    )?;
    Ok(Some(timeline))
}

fn boundary(inputs: &SimulatorInputs) -> Result<Polygon2> {
    Ok(Polygon2::new(
        inputs
            .environment
            .boundary_xy_m
            .iter()
            .copied()
            .map(Vec2::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?,
    )?)
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::mpsc::TryRecvError};

    use crate::configuration::load_simulator_inputs;

    use super::{GenerationRuntime, GenerationRuntimeError, WorkerCommand};

    fn inputs() -> crate::configuration::SimulatorInputs {
        load_simulator_inputs(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
        )
        .unwrap()
    }

    #[test]
    fn worker_failure_drains_other_responses_and_poison_runtime() {
        let mut runtime = GenerationRuntime::from_inputs(&inputs()).unwrap();
        runtime.sensors[0]
            .request
            .send(WorkerCommand::Stop)
            .unwrap();
        runtime.sensors[0].worker.take().unwrap().join().unwrap();

        assert!(matches!(
            runtime.next_completed_scans(),
            Err(GenerationRuntimeError::WorkerRequest)
        ));
        assert!(matches!(
            runtime.sensors[1].response.try_recv(),
            Err(TryRecvError::Empty)
        ));
        assert!(matches!(
            runtime.next_completed_scans(),
            Err(GenerationRuntimeError::Terminal)
        ));
        runtime.shutdown().unwrap();
    }

    #[test]
    fn one_batch_enforces_the_scenario_event_limit_atomically() {
        let mut inputs = inputs();
        inputs.simulator.scenario.surface.cell_size_m = 1.0;
        inputs.simulator.scenario.surface.update_interval_s = 1.0 / 4_097.0;
        inputs.simulator.measurement.sample_rate_hz = 4_096.0;
        inputs.simulator.measurement.rotation_rate_hz = 1.0;
        let mut runtime = GenerationRuntime::from_inputs(&inputs).unwrap();

        assert!(matches!(
            runtime.next_completed_scans(),
            Err(GenerationRuntimeError::Scenario(
                crate::scenario::ScenarioError::Resource(_)
            ))
        ));
        assert_eq!(runtime.elapsed_s(), 0.0);
        assert!(matches!(
            runtime.next_completed_scans(),
            Err(GenerationRuntimeError::Terminal)
        ));
        runtime.shutdown().unwrap();
    }
}

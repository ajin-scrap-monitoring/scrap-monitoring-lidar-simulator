use std::f64::consts::TAU;

use crate::{
    MAX_INLET_POSITIONS,
    configuration::{ScenarioConfig, SimulatorInputs},
    geometry::{Polygon2, Vec2},
    randomness::{ModelRng, RandomSource, StreamScope},
    rate_profile::{SmoothRateProfile, create_smooth_rate_profile},
};

use super::{
    HeightField, Result, ScenarioError, SurfaceSnapshot, scale_duration_range, scenario_time_scale,
};

const STATE_TOLERANCE: f64 = 1e-12;
const ROUGHNESS_CENTER_ATTEMPTS: usize = 32;
pub const MAX_EVENTS_PER_ADVANCE: usize = 4_096;
pub const MAX_EVENT_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioPhase {
    Filling,
    Collecting,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActiveScenarioPhase<'a> {
    Filling {
        cycle_index: u64,
        started_at_s: f64,
        ends_at_s: f64,
        inlet_index: usize,
        rate_profile: &'a SmoothRateProfile,
    },
    Collecting {
        cycle_index: u64,
        started_at_s: f64,
        ends_at_s: f64,
    },
}

impl ScenarioPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filling => "filling",
            Self::Collecting => "collecting",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScenarioSettings {
    mean_fill_duration_s: f64,
    fill_duration_factor_range: [f64; 2],
    fill_rate_factor_range: [f64; 2],
    fill_rate_change_duration_s_range: [f64; 2],
    collection_threshold_range: [f64; 2],
    collection_duration_factor_range: [f64; 2],
    collection_rate_factor_range: [f64; 2],
    collection_rate_change_duration_s_range: [f64; 2],
    inlet_positions: Vec<Vec2>,
    inlet_switch_activation_ratio: f64,
    inlet_switch_height_difference_m: f64,
    inlet_comparison_radius_m: f64,
    surface_update_interval_s: f64,
    pile_spread_radius_m: f64,
    roughness_height_range_m: [f64; 2],
    roughness_radius_range_m: [f64; 2],
}

impl ScenarioSettings {
    pub fn from_config(config: &ScenarioConfig) -> Result<Self> {
        validate_scenario_config(config)?;
        if config.inlet_positions_xy_m.is_empty() {
            return Err(ScenarioError::Invalid(
                "scenario must contain at least one inlet position",
            ));
        }
        if config.inlet_positions_xy_m.len() > MAX_INLET_POSITIONS {
            return Err(ScenarioError::Resource(
                "scenario inlet count exceeds the engine limit",
            ));
        }
        let time_scale = scenario_time_scale(config.mean_fill_duration_s)?;
        let inlet_positions = config
            .inlet_positions_xy_m
            .iter()
            .copied()
            .map(Vec2::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (index, inlet) in inlet_positions.iter().enumerate() {
            if inlet_positions[..index].contains(inlet) {
                return Err(ScenarioError::Invalid(
                    "scenario inlet positions must be unique",
                ));
            }
        }
        Ok(Self {
            mean_fill_duration_s: config.mean_fill_duration_s,
            fill_duration_factor_range: config.fill_duration_factor_range,
            fill_rate_factor_range: config.fill_rate_factor_range,
            fill_rate_change_duration_s_range: scale_duration_range(
                config.fill_rate_change_duration_s_range,
                time_scale,
            )?,
            collection_threshold_range: config.collection_threshold_range,
            collection_duration_factor_range: config.collection_duration_factor_range,
            collection_rate_factor_range: config.collection_rate_factor_range,
            collection_rate_change_duration_s_range: scale_duration_range(
                config.collection_rate_change_duration_s_range,
                time_scale,
            )?,
            inlet_positions,
            inlet_switch_activation_ratio: config.inlet_switch_activation_ratio,
            inlet_switch_height_difference_m: config.inlet_switch_height_difference_m,
            inlet_comparison_radius_m: config.inlet_comparison_radius_m,
            surface_update_interval_s: config.surface.update_interval_s,
            pile_spread_radius_m: config.surface.pile_spread_radius_m,
            roughness_height_range_m: config.surface.roughness_height_range_m,
            roughness_radius_range_m: config.surface.roughness_radius_range_m,
        })
    }

    pub fn surface_update_interval_s(&self) -> f64 {
        self.surface_update_interval_s
    }

    pub fn fill_rate_change_duration_s_range(&self) -> [f64; 2] {
        self.fill_rate_change_duration_s_range
    }

    pub fn collection_rate_change_duration_s_range(&self) -> [f64; 2] {
        self.collection_rate_change_duration_s_range
    }
}

#[derive(Clone, Debug)]
struct FillPlan {
    started_at_s: f64,
    duration_s: f64,
    target_fill_ratio: f64,
    target_volume_m3: f64,
    rate_profile: SmoothRateProfile,
}

impl FillPlan {
    fn ends_at_s(&self) -> f64 {
        self.started_at_s + self.duration_s
    }

    fn average_rate_m3_per_s(&self) -> f64 {
        self.target_volume_m3 / self.duration_s
    }
}

#[derive(Clone, Debug)]
struct CollectionPlan {
    started_at_s: f64,
    duration_s: f64,
    starting_volume_m3: f64,
    rate_profile: SmoothRateProfile,
}

impl CollectionPlan {
    fn ends_at_s(&self) -> f64 {
        self.started_at_s + self.duration_s
    }

    fn average_rate_m3_per_s(&self) -> f64 {
        self.starting_volume_m3 / self.duration_s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioSnapshot {
    pub elapsed_s: f64,
    pub surface_updated_at_s: f64,
    pub cycle_index: u64,
    pub phase: ScenarioPhase,
    pub phase_started_at_s: f64,
    pub phase_ends_at_s: f64,
    pub phase_duration_s: f64,
    pub rate_factor: f64,
    pub target_fill_ratio: f64,
    pub surface_fill_ratio: f64,
    pub surface_volume_m3: f64,
    pub current_inlet_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ScenarioModelSnapshot {
    pub state: ScenarioSnapshot,
    pub surface: SurfaceSnapshot,
}

#[derive(Clone, Debug)]
pub struct ScenarioEvent {
    pub elapsed_s: f64,
    pub surface_update_due: bool,
    pub phase_transition_due: bool,
    pub model: ScenarioModelSnapshot,
}

#[derive(Clone, Debug)]
pub struct ScenarioAdvance {
    pub state: ScenarioSnapshot,
    pub events: Vec<ScenarioEvent>,
}

#[derive(Clone, Debug)]
pub struct ScenarioSimulator {
    surface: HeightField,
    settings: ScenarioSettings,
    cycle_rng: ModelRng,
    fill_rate_rng: ModelRng,
    collection_rate_rng: ModelRng,
    roughness_rng: ModelRng,
    elapsed_s: f64,
    surface_updated_at_s: f64,
    next_update_index: u64,
    cycle_index: u64,
    phase: ScenarioPhase,
    current_inlet_index: usize,
    fill_plan: FillPlan,
    collection_plan: Option<CollectionPlan>,
}

impl ScenarioSimulator {
    pub fn new(surface: HeightField, settings: ScenarioSettings, seed: u64) -> Result<Self> {
        if surface.volume_m3() > state_volume_tolerance(&surface) {
            return Err(ScenarioError::Invalid(
                "scenario surface must be empty at simulation start",
            ));
        }
        for inlet in &settings.inlet_positions {
            if !surface.boundary().contains(*inlet)? {
                return Err(ScenarioError::Invalid(
                    "scenario inlet positions must lie inside the surface boundary",
                ));
            }
        }
        let mut simulator = Self {
            surface,
            settings,
            cycle_rng: ModelRng::new(seed, StreamScope::Global, "cycle-plan")?,
            fill_rate_rng: ModelRng::new(seed, StreamScope::Global, "fill-rate-profile")?,
            collection_rate_rng: ModelRng::new(
                seed,
                StreamScope::Global,
                "collection-rate-profile",
            )?,
            roughness_rng: ModelRng::new(seed, StreamScope::Global, "surface-roughness")?,
            elapsed_s: 0.0,
            surface_updated_at_s: 0.0,
            next_update_index: 1,
            cycle_index: 0,
            phase: ScenarioPhase::Filling,
            current_inlet_index: 0,
            fill_plan: empty_fill_plan(),
            collection_plan: None,
        };
        simulator.fill_plan = simulator.create_fill_plan(0.0)?;
        Ok(simulator)
    }

    pub fn surface(&self) -> &HeightField {
        &self.surface
    }

    pub fn elapsed_s(&self) -> f64 {
        self.elapsed_s
    }

    pub fn active_phase(&self) -> Result<ActiveScenarioPhase<'_>> {
        match self.phase {
            ScenarioPhase::Filling => Ok(ActiveScenarioPhase::Filling {
                cycle_index: self.cycle_index,
                started_at_s: self.fill_plan.started_at_s,
                ends_at_s: self.fill_plan.ends_at_s(),
                inlet_index: self.current_inlet_index,
                rate_profile: &self.fill_plan.rate_profile,
            }),
            ScenarioPhase::Collecting => {
                let plan = self
                    .collection_plan
                    .as_ref()
                    .ok_or(ScenarioError::State("collection phase has no active plan"))?;
                Ok(ActiveScenarioPhase::Collecting {
                    cycle_index: self.cycle_index,
                    started_at_s: plan.started_at_s,
                    ends_at_s: plan.ends_at_s(),
                })
            }
        }
    }

    pub fn next_surface_event_elapsed_s(&self) -> Result<f64> {
        self.next_event()
    }

    pub fn snapshot(&self) -> Result<ScenarioSnapshot> {
        let (started_at_s, ends_at_s, duration_s, rate_profile) = match self.phase {
            ScenarioPhase::Filling => (
                self.fill_plan.started_at_s,
                self.fill_plan.ends_at_s(),
                self.fill_plan.duration_s,
                &self.fill_plan.rate_profile,
            ),
            ScenarioPhase::Collecting => {
                let plan = self
                    .collection_plan
                    .as_ref()
                    .ok_or(ScenarioError::State("collection phase has no active plan"))?;
                (
                    plan.started_at_s,
                    plan.ends_at_s(),
                    plan.duration_s,
                    &plan.rate_profile,
                )
            }
        };
        let phase_elapsed_s = (self.elapsed_s - started_at_s).clamp(0.0, duration_s);
        Ok(ScenarioSnapshot {
            elapsed_s: self.elapsed_s,
            surface_updated_at_s: self.surface_updated_at_s,
            cycle_index: self.cycle_index,
            phase: self.phase,
            phase_started_at_s: started_at_s,
            phase_ends_at_s: ends_at_s,
            phase_duration_s: duration_s,
            rate_factor: rate_profile.factor_at(phase_elapsed_s)?,
            target_fill_ratio: self.fill_plan.target_fill_ratio,
            surface_fill_ratio: self.surface.fill_ratio(),
            surface_volume_m3: self.surface.volume_m3(),
            current_inlet_index: (self.phase == ScenarioPhase::Filling)
                .then_some(self.current_inlet_index),
        })
    }

    pub fn observation_snapshot(&self) -> Result<ScenarioModelSnapshot> {
        Ok(ScenarioModelSnapshot {
            state: self.snapshot()?,
            surface: self.surface.surface_snapshot()?,
        })
    }

    pub fn advance_to(&mut self, elapsed_s: f64) -> Result<ScenarioAdvance> {
        if !elapsed_s.is_finite() || elapsed_s < self.elapsed_s {
            return Err(ScenarioError::Invalid(
                "scenario elapsed time must be finite and monotonic",
            ));
        }
        self.require_bounded_update_output(elapsed_s)?;
        let mut candidate = self.clone();
        let result = candidate.advance_inner(elapsed_s)?;
        *self = candidate;
        Ok(result)
    }

    fn require_bounded_update_output(&self, elapsed_s: f64) -> Result<()> {
        let event_limit = self.event_output_limit()?;
        let last_update_index = (elapsed_s / self.settings.surface_update_interval_s).floor();
        let completed_update_index = self.next_update_index.saturating_sub(1) as f64;
        if !last_update_index.is_finite()
            || last_update_index - completed_update_index > event_limit as f64
        {
            return Err(ScenarioError::Resource(
                "scenario advance exceeds the event output limit",
            ));
        }
        Ok(())
    }

    fn advance_inner(&mut self, elapsed_s: f64) -> Result<ScenarioAdvance> {
        let event_limit = self.event_output_limit()?;
        let mut events = Vec::new();
        events
            .try_reserve_exact(event_limit)
            .map_err(|_| ScenarioError::Resource("scenario event allocation failed"))?;
        loop {
            let next_event_s = self.next_event()?;
            if next_event_s > elapsed_s {
                break;
            }
            if events.len() == event_limit {
                return Err(ScenarioError::Resource(
                    "scenario advance exceeds the event output limit",
                ));
            }
            let next_update_s = self.next_update_time()?;
            let phase_end_s = self.phase_end_s()?;
            let update_due = next_event_s == next_update_s;
            let transition_due = next_event_s == phase_end_s;

            self.apply_phase_change(next_event_s)?;
            self.surface_updated_at_s = next_event_s;
            self.elapsed_s = next_event_s;
            if update_due {
                self.next_update_index =
                    self.next_update_index
                        .checked_add(1)
                        .ok_or(ScenarioError::Resource(
                            "scenario update index exceeds the engine limit",
                        ))?;
            }
            if transition_due {
                self.transition_phase(next_event_s)?;
            } else if self.phase == ScenarioPhase::Filling {
                self.switch_inlet_if_needed()?;
            }
            events.push(ScenarioEvent {
                elapsed_s: next_event_s,
                surface_update_due: update_due,
                phase_transition_due: transition_due,
                model: self.observation_snapshot()?,
            });
        }
        self.elapsed_s = elapsed_s;
        Ok(ScenarioAdvance {
            state: self.snapshot()?,
            events,
        })
    }

    pub(crate) fn event_output_limit(&self) -> Result<usize> {
        let bytes_per_snapshot = self
            .surface
            .heights_m()
            .len()
            .checked_mul(std::mem::size_of::<f64>())
            .ok_or(ScenarioError::Resource(
                "scenario snapshot size exceeds the engine limit",
            ))?;
        if bytes_per_snapshot == 0 {
            return Err(ScenarioError::State(
                "scenario surface must contain height nodes",
            ));
        }
        Ok(MAX_EVENTS_PER_ADVANCE.min(MAX_EVENT_SNAPSHOT_BYTES / bytes_per_snapshot))
    }

    fn next_update_time(&self) -> Result<f64> {
        let value = self.settings.surface_update_interval_s * self.next_update_index as f64;
        if !value.is_finite() || value <= self.surface_updated_at_s {
            return Err(ScenarioError::State(
                "scenario update schedule cannot advance finitely",
            ));
        }
        Ok(value)
    }

    fn phase_end_s(&self) -> Result<f64> {
        let value = match self.phase {
            ScenarioPhase::Filling => self.fill_plan.ends_at_s(),
            ScenarioPhase::Collecting => self
                .collection_plan
                .as_ref()
                .ok_or(ScenarioError::State("collection phase has no active plan"))?
                .ends_at_s(),
        };
        if !value.is_finite() || value <= self.surface_updated_at_s {
            return Err(ScenarioError::State(
                "scenario phase schedule cannot advance finitely",
            ));
        }
        Ok(value)
    }

    fn next_event(&self) -> Result<f64> {
        Ok(self.next_update_time()?.min(self.phase_end_s()?))
    }

    fn create_fill_plan(&mut self, started_at_s: f64) -> Result<FillPlan> {
        let duration_factor = self.cycle_rng.uniform(
            self.settings.fill_duration_factor_range[0],
            self.settings.fill_duration_factor_range[1],
        )?;
        let duration_s = self.settings.mean_fill_duration_s * duration_factor;
        require_phase_end(started_at_s, duration_s)?;
        let target_fill_ratio = self.cycle_rng.uniform(
            self.settings.collection_threshold_range[0],
            self.settings.collection_threshold_range[1],
        )?;
        Ok(FillPlan {
            started_at_s,
            duration_s,
            target_fill_ratio,
            target_volume_m3: self.surface.capacity_m3() * target_fill_ratio,
            rate_profile: create_smooth_rate_profile(
                duration_s,
                self.settings.fill_rate_factor_range,
                self.settings.fill_rate_change_duration_s_range,
                &mut self.fill_rate_rng,
            )?,
        })
    }

    fn create_collection_plan(&mut self, started_at_s: f64) -> Result<CollectionPlan> {
        let duration_factor = self.cycle_rng.uniform(
            self.settings.collection_duration_factor_range[0],
            self.settings.collection_duration_factor_range[1],
        )?;
        let duration_s = self.settings.mean_fill_duration_s * duration_factor;
        require_phase_end(started_at_s, duration_s)?;
        Ok(CollectionPlan {
            started_at_s,
            duration_s,
            starting_volume_m3: self.surface.volume_m3(),
            rate_profile: create_smooth_rate_profile(
                duration_s,
                self.settings.collection_rate_factor_range,
                self.settings.collection_rate_change_duration_s_range,
                &mut self.collection_rate_rng,
            )?,
        })
    }

    fn apply_phase_change(&mut self, through_s: f64) -> Result<()> {
        let (started_at_s, duration_s, ends_at_s, average_rate, integrated_factor_s, filling) =
            match self.phase {
                ScenarioPhase::Filling => {
                    let plan = &self.fill_plan;
                    (
                        plan.started_at_s,
                        plan.duration_s,
                        plan.ends_at_s(),
                        plan.average_rate_m3_per_s(),
                        plan.rate_profile.integrated_factor_between(
                            (self.surface_updated_at_s - plan.started_at_s).max(0.0),
                            (through_s - plan.started_at_s).min(plan.duration_s),
                        )?,
                        true,
                    )
                }
                ScenarioPhase::Collecting => {
                    let plan = self
                        .collection_plan
                        .as_ref()
                        .ok_or(ScenarioError::State("collection phase has no active plan"))?;
                    (
                        plan.started_at_s,
                        plan.duration_s,
                        plan.ends_at_s(),
                        plan.average_rate_m3_per_s(),
                        plan.rate_profile.integrated_factor_between(
                            (self.surface_updated_at_s - plan.started_at_s).max(0.0),
                            (through_s - plan.started_at_s).min(plan.duration_s),
                        )?,
                        false,
                    )
                }
            };
        debug_assert!(through_s >= started_at_s && through_s <= started_at_s + duration_s);

        let change = if filling {
            let requested_m3 = if through_s == ends_at_s {
                (self.fill_plan.target_volume_m3 - self.surface.volume_m3()).max(0.0)
            } else {
                average_rate * integrated_factor_s
            };
            let change = self.surface.add_volume(
                requested_m3,
                self.settings.inlet_positions[self.current_inlet_index],
                self.settings.pile_spread_radius_m,
            )?;
            self.apply_local_roughness()?;
            change
        } else {
            let requested_m3 = if through_s == ends_at_s {
                self.surface.volume_m3()
            } else {
                self.surface
                    .volume_m3()
                    .min(average_rate * integrated_factor_s)
            };
            self.surface.remove_volume_uniformly(requested_m3)?
        };
        self.surface.relax_slopes()?;
        if change.unapplied_m3() > state_volume_tolerance(&self.surface) {
            return Err(ScenarioError::State(
                "scenario surface could not apply the planned volume change",
            ));
        }
        Ok(())
    }

    fn apply_local_roughness(&mut self) -> Result<()> {
        let peak_delta_m = self.roughness_rng.uniform(
            self.settings.roughness_height_range_m[0],
            self.settings.roughness_height_range_m[1],
        )?;
        if peak_delta_m == 0.0 {
            return Ok(());
        }
        let radius_m = self.roughness_rng.uniform(
            self.settings.roughness_radius_range_m[0],
            self.settings.roughness_radius_range_m[1],
        )?;
        let center = self.sample_roughness_center()?;
        self.surface
            .apply_local_roughness(center, radius_m, peak_delta_m)?;
        Ok(())
    }

    fn sample_roughness_center(&mut self) -> Result<Vec2> {
        let inlet = self.settings.inlet_positions[self.current_inlet_index];
        for _ in 0..ROUGHNESS_CENTER_ATTEMPTS {
            let distance_m =
                self.settings.pile_spread_radius_m * self.roughness_rng.unit_f64().sqrt();
            let angle_rad = self.roughness_rng.unit_f64() * TAU;
            let candidate = Vec2::new(
                inlet.x() + distance_m * angle_rad.cos(),
                inlet.y() + distance_m * angle_rad.sin(),
            )?;
            if self.surface.boundary().contains(candidate)? {
                return Ok(candidate);
            }
        }
        Ok(inlet)
    }

    fn transition_phase(&mut self, transitioned_at_s: f64) -> Result<()> {
        let tolerance = state_volume_tolerance(&self.surface);
        match self.phase {
            ScenarioPhase::Filling => {
                if (self.surface.volume_m3() - self.fill_plan.target_volume_m3).abs() > tolerance {
                    return Err(ScenarioError::State(
                        "fill phase did not reach its planned threshold volume",
                    ));
                }
                self.phase = ScenarioPhase::Collecting;
                self.collection_plan = Some(self.create_collection_plan(transitioned_at_s)?);
            }
            ScenarioPhase::Collecting => {
                if self.surface.volume_m3() > tolerance {
                    return Err(ScenarioError::State(
                        "collection phase did not empty the surface",
                    ));
                }
                self.cycle_index =
                    self.cycle_index
                        .checked_add(1)
                        .ok_or(ScenarioError::Resource(
                            "scenario cycle index exceeds the engine limit",
                        ))?;
                self.phase = ScenarioPhase::Filling;
                self.current_inlet_index = 0;
                self.collection_plan = None;
                self.fill_plan = self.create_fill_plan(transitioned_at_s)?;
            }
        }
        Ok(())
    }

    fn switch_inlet_if_needed(&mut self) -> Result<()> {
        if self.surface.fill_ratio() < self.settings.inlet_switch_activation_ratio {
            return Ok(());
        }
        let mut lowest_index = 0;
        let mut lowest_height = f64::INFINITY;
        let mut current_height = None;
        for (index, inlet) in self.settings.inlet_positions.iter().enumerate() {
            let height = self
                .surface
                .mean_height_within(*inlet, self.settings.inlet_comparison_radius_m)?;
            if index == self.current_inlet_index {
                current_height = Some(height);
            }
            if height < lowest_height {
                lowest_index = index;
                lowest_height = height;
            }
        }
        let current_height = current_height.ok_or(ScenarioError::State(
            "scenario current inlet index is invalid",
        ))?;
        if lowest_index != self.current_inlet_index
            && current_height > lowest_height
            && current_height - lowest_height >= self.settings.inlet_switch_height_difference_m
        {
            self.current_inlet_index = lowest_index;
        }
        Ok(())
    }
}

pub fn build_scenario_simulator(inputs: &SimulatorInputs) -> Result<ScenarioSimulator> {
    let boundary = Polygon2::new(
        inputs
            .environment
            .boundary_xy_m
            .iter()
            .copied()
            .map(Vec2::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()?,
    )?;
    let surface = HeightField::new(
        boundary,
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        inputs.simulator.scenario.surface.cell_size_m,
    )?;
    let settings = ScenarioSettings::from_config(&inputs.simulator.scenario)?;
    ScenarioSimulator::new(surface, settings, inputs.simulator.seed)
}

fn require_phase_end(started_at_s: f64, duration_s: f64) -> Result<()> {
    let ends_at_s = started_at_s + duration_s;
    if !started_at_s.is_finite()
        || !duration_s.is_finite()
        || duration_s <= 0.0
        || !ends_at_s.is_finite()
        || ends_at_s <= started_at_s
    {
        return Err(ScenarioError::State(
            "scenario phase schedule cannot advance finitely",
        ));
    }
    Ok(())
}

fn state_volume_tolerance(surface: &HeightField) -> f64 {
    surface.capacity_m3().max(1.0) * STATE_TOLERANCE
}

fn validate_scenario_config(config: &ScenarioConfig) -> Result<()> {
    require_positive(config.mean_fill_duration_s, "mean fill duration")?;
    require_range(
        config.fill_duration_factor_range,
        "fill duration factor range",
        true,
    )?;
    if (config.fill_duration_factor_range[0] + config.fill_duration_factor_range[1] - 2.0).abs()
        > STATE_TOLERANCE
    {
        return Err(ScenarioError::Invalid(
            "fill duration factor range must be centered on 1",
        ));
    }
    require_average_range(config.fill_rate_factor_range, "fill rate factor range")?;
    require_range(
        config.fill_rate_change_duration_s_range,
        "fill rate change duration range",
        true,
    )?;
    require_range(
        config.collection_threshold_range,
        "collection threshold range",
        true,
    )?;
    if config.collection_threshold_range[1] > 1.0 {
        return Err(ScenarioError::Invalid(
            "collection threshold range must not exceed 1",
        ));
    }
    require_range(
        config.collection_duration_factor_range,
        "collection duration factor range",
        true,
    )?;
    require_average_range(
        config.collection_rate_factor_range,
        "collection rate factor range",
    )?;
    require_range(
        config.collection_rate_change_duration_s_range,
        "collection rate change duration range",
        true,
    )?;
    require_ratio(
        config.inlet_switch_activation_ratio,
        "inlet switch activation ratio",
    )?;
    require_non_negative(
        config.inlet_switch_height_difference_m,
        "inlet switch height difference",
    )?;
    require_positive(config.inlet_comparison_radius_m, "inlet comparison radius")?;
    require_positive(config.surface.cell_size_m, "surface cell size")?;
    require_positive(config.surface.update_interval_s, "surface update interval")?;
    require_positive(config.surface.pile_spread_radius_m, "pile spread radius")?;
    require_range(
        config.surface.roughness_height_range_m,
        "roughness height range",
        false,
    )?;
    require_range(
        config.surface.roughness_radius_range_m,
        "roughness radius range",
        true,
    )?;
    let minimum_fill_duration = config.mean_fill_duration_s * config.fill_duration_factor_range[0];
    let maximum_fill_duration = config.mean_fill_duration_s * config.fill_duration_factor_range[1];
    let minimum_collection_duration =
        config.mean_fill_duration_s * config.collection_duration_factor_range[0];
    let maximum_collection_duration =
        config.mean_fill_duration_s * config.collection_duration_factor_range[1];
    if !minimum_fill_duration.is_finite()
        || minimum_fill_duration <= 0.0
        || !maximum_fill_duration.is_finite()
        || !minimum_collection_duration.is_finite()
        || minimum_collection_duration <= 0.0
        || !maximum_collection_duration.is_finite()
        || maximum_fill_duration + maximum_collection_duration <= maximum_fill_duration
        || !(maximum_fill_duration + maximum_collection_duration).is_finite()
    {
        return Err(ScenarioError::Invalid(
            "scenario phase durations cannot advance finitely",
        ));
    }
    Ok(())
}

fn require_range(value: [f64; 2], name: &'static str, positive: bool) -> Result<()> {
    if !value[0].is_finite()
        || !value[1].is_finite()
        || value[1] < value[0]
        || (positive && value[0] <= 0.0)
    {
        return Err(ScenarioError::Invalid(name));
    }
    Ok(())
}

fn require_average_range(value: [f64; 2], name: &'static str) -> Result<()> {
    require_range(value, name, true)?;
    if value[0] > 1.0 || value[1] < 1.0 {
        return Err(ScenarioError::Invalid(name));
    }
    Ok(())
}

fn require_positive(value: f64, name: &'static str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        return Err(ScenarioError::Invalid(name));
    }
    Ok(())
}

fn require_non_negative(value: f64, name: &'static str) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(ScenarioError::Invalid(name));
    }
    Ok(())
}

fn require_ratio(value: f64, name: &'static str) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ScenarioError::Invalid(name));
    }
    Ok(())
}

fn empty_fill_plan() -> FillPlan {
    FillPlan {
        started_at_s: 0.0,
        duration_s: 1.0,
        target_fill_ratio: 0.0,
        target_volume_m3: 0.0,
        rate_profile: SmoothRateProfile::new(1.0, Vec::new())
            .expect("the internal empty rate profile is valid"),
    }
}
